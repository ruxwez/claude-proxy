// Package opencode implementa los providers "opencode" y "opencode-responses"
// para los modelos de OpenCode Zen (incluidos los gratuitos).
//
//   - "opencode": OpenAI Chat Completions contra {base_url}/chat/completions.
//     Cubre todos los modelos gratuitos excepto muse-spark contributor.
//   - "opencode-responses": OpenAI Responses API contra {base_url}/responses.
//     Cubre muse-spark-1.x-contributor-free.
//
// Base por defecto: https://opencode.ai/zen/v1 (Zen). Para OpenCode Go usar
// base_url = "https://opencode.ai/zen/go/v1" explícito.
// Auth: api_key / api_key_env = OPENCODE_API_KEY (Bearer).
package opencode

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"time"

	"github.com/ruxwez/claude-proxy/internal/adapter"
	"github.com/ruxwez/claude-proxy/internal/anthropic"
	"github.com/ruxwez/claude-proxy/internal/config"
	oai "github.com/ruxwez/claude-proxy/internal/openai"
	"github.com/ruxwez/claude-proxy/internal/translate"
)

func init() {
	adapter.Register(&ChatAdapter{})
	adapter.Register(&ResponsesAdapter{})
}

// ChatAdapter implementa provider "opencode" vía /chat/completions.
type ChatAdapter struct{}

func (a *ChatAdapter) Provider() string { return "opencode" }

func (a *ChatAdapter) Handle(ctx context.Context, anthReq *anthropic.MessagesRequest, route config.ResolvedRoute, w http.ResponseWriter, r *http.Request) error {
	chatReq, err := translate.AnthropicToOpenAI(anthReq, route.TargetModel)
	if err != nil {
		return err
	}
	payload, err := json.Marshal(chatReq)
	if err != nil {
		return fmt.Errorf("serializar petición opencode: %w", err)
	}
	resp, err := doPost(ctx, route, r, "/chat/completions", payload)
	if err != nil {
		return err
	}
	defer resp.Body.Close()

	if anthReq.Stream {
		if resp.StatusCode < 200 || resp.StatusCode >= 300 {
			return writeProviderError(w, resp, "opencode")
		}
		writeSSEHeaders(w)
		return translate.OpenAIStreamToAnthropic(resp.Body, w, anthReq.Model)
	}
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return writeProviderError(w, resp, "opencode")
	}
	var chatResp oai.ChatResponse
	if err := json.NewDecoder(resp.Body).Decode(&chatResp); err != nil {
		return fmt.Errorf("decodificar respuesta opencode: %w", err)
	}
	anthResp, err := translate.OpenAIToAnthropic(&chatResp, anthReq.Model)
	if err != nil {
		return err
	}
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(http.StatusOK)
	return json.NewEncoder(w).Encode(anthResp)
}

// doPost envía el POST al endpoint Zen con las cabeceras requeridas.
// Usa http.Client{Timeout} (no context cancelado al retorno) para no
// abortar el streaming mientras el caller consume resp.Body.
func doPost(ctx context.Context, route config.ResolvedRoute, r *http.Request, path string, payload []byte) (*http.Response, error) {
	timeout := time.Duration(route.TimeoutSeconds) * time.Second
	if timeout <= 0 {
		timeout = 120 * time.Second
	}

	url := route.BaseURL + path
	httpReq, err := http.NewRequestWithContext(ctx, http.MethodPost, url, bytes.NewReader(payload))
	if err != nil {
		return nil, err
	}
	httpReq.Header.Set("Content-Type", "application/json")
	if route.APIKey != "" {
		httpReq.Header.Set("Authorization", "Bearer "+route.APIKey)
	}
	// Zen/Go recomienda UA propio y sesión estable para routing y caché.
	if httpReq.Header.Get("User-Agent") == "" {
		httpReq.Header.Set("User-Agent", "claude-proxy/1.0")
	}
	if sid := r.Header.Get("x-opencode-session"); sid != "" {
		httpReq.Header.Set("x-opencode-session", sid)
	}
	for k, v := range route.ExtraHeaders {
		httpReq.Header.Set(k, v)
	}
	client := &http.Client{Timeout: timeout}
	resp, err := client.Do(httpReq)
	if err != nil {
		return nil, fmt.Errorf("proveedor opencode no alcanzable: %w", err)
	}
	return resp, nil
}

func writeSSEHeaders(w http.ResponseWriter) {
	h := w.Header()
	h.Set("Content-Type", "text/event-stream")
	h.Set("Cache-Control", "no-cache")
	h.Set("Connection", "keep-alive")
	h.Set("X-Accel-Buffering", "no")
	w.WriteHeader(http.StatusOK)
}

// writeProviderError reenvía el error del proveedor en formato Anthropic.
func writeProviderError(w http.ResponseWriter, resp *http.Response, provider string) error {
	raw, _ := io.ReadAll(io.LimitReader(resp.Body, 1<<20))
	msg := stringsTrim(string(raw))
	if msg == "" {
		msg = "error del proveedor " + provider + ": status " + resp.Status
	} else {
		var parsed oai.ErrorResponse
		if err := json.Unmarshal(raw, &parsed); err == nil && parsed.Error.Message != "" {
			msg = parsed.Error.Message
		} else if len(msg) > 2000 {
			msg = msg[:2000]
		}
	}
	status := resp.StatusCode
	if status < 400 || status >= 600 {
		status = http.StatusBadGateway
	}
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	_ = json.NewEncoder(w).Encode(anthropic.NewError("api_error", msg))
	return nil
}

func stringsTrim(s string) string {
	out := s
	for len(out) > 0 && (out[0] == ' ' || out[0] == '\n' || out[0] == '\r' || out[0] == '\t') {
		out = out[1:]
	}
	for len(out) > 0 {
		c := out[len(out)-1]
		if c == ' ' || c == '\n' || c == '\r' || c == '\t' {
			out = out[:len(out)-1]
		} else {
			break
		}
	}
	return out
}
