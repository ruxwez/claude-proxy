package openai

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
	adapter.Register(&Adapter{})
}

// Adapter implementa provider "openai" vía /chat/completions.
type Adapter struct{}

func (a *Adapter) Provider() string { return "openai" }

func (a *Adapter) Handle(ctx context.Context, anthReq *anthropic.MessagesRequest, route config.ResolvedRoute, w http.ResponseWriter, r *http.Request) error {
	chatReq, err := translate.AnthropicToOpenAI(anthReq, route.TargetModel)
	if err != nil {
		return err
	}

	payload, err := json.Marshal(chatReq)
	if err != nil {
		return fmt.Errorf("serializar petición OpenAI: %w", err)
	}

	timeout := time.Duration(route.TimeoutSeconds) * time.Second
	if timeout <= 0 {
		timeout = 120 * time.Second
	}
	ctx, cancel := context.WithTimeout(ctx, timeout)
	defer cancel()

	url := route.BaseURL + "/chat/completions"
	httpReq, err := http.NewRequestWithContext(ctx, http.MethodPost, url, bytes.NewReader(payload))
	if err != nil {
		return err
	}
	httpReq.Header.Set("Content-Type", "application/json")
	if route.APIKey != "" {
		httpReq.Header.Set("Authorization", "Bearer "+route.APIKey)
	}
	for k, v := range route.ExtraHeaders {
		httpReq.Header.Set(k, v)
	}

	client := &http.Client{}
	resp, err := client.Do(httpReq)
	if err != nil {
		return fmt.Errorf("proveedor openai no alcanzable: %w", err)
	}
	defer resp.Body.Close()

	if anthReq.Stream {
		if resp.StatusCode < 200 || resp.StatusCode >= 300 {
			return writeProviderError(w, resp)
		}
		h := w.Header()
		h.Set("Content-Type", "text/event-stream")
		h.Set("Cache-Control", "no-cache")
		h.Set("Connection", "keep-alive")
		h.Set("X-Accel-Buffering", "no")
		w.WriteHeader(http.StatusOK)
		return translate.OpenAIStreamToAnthropic(resp.Body, w, anthReq.Model)
	}

	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return writeProviderError(w, resp)
	}
	var chatResp oai.ChatResponse
	if err := json.NewDecoder(resp.Body).Decode(&chatResp); err != nil {
		return fmt.Errorf("decodificar respuesta OpenAI: %w", err)
	}
	anthResp, err := translate.OpenAIToAnthropic(&chatResp, anthReq.Model)
	if err != nil {
		return err
	}
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(http.StatusOK)
	return json.NewEncoder(w).Encode(anthResp)
}

// writeProviderError reenvía el error del proveedor en formato Anthropic.
func writeProviderError(w http.ResponseWriter, resp *http.Response) error {
	raw, _ := io.ReadAll(io.LimitReader(resp.Body, 1<<20))
	msg := strings_trim(string(raw))
	if msg == "" {
		msg = "error del proveedor openai: status " + resp.Status
	} else {
		// Intentar extraer .error.message de OpenAI.
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

func strings_trim(s string) string {
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
