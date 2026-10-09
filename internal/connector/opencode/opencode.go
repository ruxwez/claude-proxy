// Package opencode implementa el conector para OpenCode Zen y OpenCode Go.
//
// Ambos comparten la misma API key (OPENCODE_API_KEY) y hablan el estándar
// OpenAI. La diferencia es la base URL y el filtrado de modelos:
//
//   - Zen  (https://opencode.ai/zen/v1): solo expone modelos gratuitos
//     (IDs que contienen "free").
//   - Go   (https://opencode.ai/zen/go/v1): expone todo el catálogo.
//
// Los modelos de tipo "muse-spark" solo se sirven por la Responses API,
// así que se enrutan a /responses; el resto usa /chat/completions.
package opencode

import (
	"context"
	"crypto/rand"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"sort"
	"strings"
	"time"

	"github.com/ruxwez/claude-proxy/internal/anthropic"
	"github.com/ruxwez/claude-proxy/internal/config"
	"github.com/ruxwez/claude-proxy/internal/connector"
	oai "github.com/ruxwez/claude-proxy/internal/openai"
	"github.com/ruxwez/claude-proxy/internal/responses"
	"github.com/ruxwez/claude-proxy/internal/translate"
)

// modelCacheTTL es cuánto se reutiliza la lista de modelos entre llamadas.
const modelCacheTTL = 5 * time.Minute

// Connector es un conector OpenCode (Zen o Go).
type Connector struct {
	name       string
	baseURL    string
	apiKey     string
	filterFree bool
	client     *http.Client
	cache      *connector.ModelCache
}

// NewZen crea el conector de OpenCode Zen (solo modelos gratuitos).
func NewZen(apiKey string, timeout time.Duration) *Connector {
	return newConnector("opencode-zen", config.ZenBaseURL, apiKey, true, timeout)
}

// NewGo crea el conector de OpenCode Go (catálogo completo).
func NewGo(apiKey string, timeout time.Duration) *Connector {
	return newConnector("opencode-go", config.GoBaseURL, apiKey, false, timeout)
}

func newConnector(name, baseURL, apiKey string, filterFree bool, timeout time.Duration) *Connector {
	if timeout <= 0 {
		timeout = 120 * time.Second
	}
	return &Connector{
		name:       name,
		baseURL:    strings.TrimRight(baseURL, "/"),
		apiKey:     apiKey,
		filterFree: filterFree,
		client:     &http.Client{Timeout: timeout},
		cache:      connector.NewModelCache(modelCacheTTL),
	}
}

// Name devuelve el prefijo del adaptador ("opencode-zen" u "opencode-go").
func (c *Connector) Name() string { return c.name }

// Models lista los modelos del proveedor (con caché de 5 minutos).
func (c *Connector) Models(ctx context.Context) ([]connector.Model, error) {
	if models, ok := c.cache.Get(); ok {
		return models, nil
	}
	models, err := c.fetchModels(ctx)
	if err != nil {
		return nil, err
	}
	c.cache.Set(models)
	return models, nil
}

func (c *Connector) fetchModels(ctx context.Context) ([]connector.Model, error) {
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, c.baseURL+"/models", nil)
	if err != nil {
		return nil, err
	}
	if c.apiKey != "" {
		req.Header.Set("Authorization", "Bearer "+c.apiKey)
	}
	req.Header.Set("Accept", "application/json")

	resp, err := c.client.Do(req)
	if err != nil {
		return nil, fmt.Errorf("%s: no se pudo listar modelos: %w", c.name, err)
	}
	defer resp.Body.Close()
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return nil, fmt.Errorf("%s: /models devolvió %s", c.name, resp.Status)
	}

	var parsed struct {
		Data []struct {
			ID      string `json:"id"`
			Created int64  `json:"created"`
		} `json:"data"`
	}
	if err := json.NewDecoder(resp.Body).Decode(&parsed); err != nil {
		return nil, fmt.Errorf("%s: respuesta /models inválida: %w", c.name, err)
	}

	out := make([]connector.Model, 0, len(parsed.Data))
	for _, m := range parsed.Data {
		id := strings.TrimSpace(m.ID)
		if id == "" {
			continue
		}
		if c.filterFree && !strings.Contains(strings.ToLower(id), "free") {
			continue
		}
		out = append(out, connector.Model{
			ID:          id,
			DisplayName: id,
			Created:     m.Created,
		})
	}
	sort.Slice(out, func(i, j int) bool { return out[i].ID < out[j].ID })
	return out, nil
}

// protocol es el estándar que habla un modelo en OpenCode.
type protocol string

const (
	protoChat      protocol = "chat"      // OpenAI Chat Completions
	protoMessages  protocol = "messages"  // Anthropic Messages nativo
	protoResponses protocol = "responses" // OpenAI Responses API
)

// Chat traduce la petición y la envía al proveedor por el protocolo adecuado.
func (c *Connector) Chat(ctx context.Context, req *anthropic.MessagesRequest, targetModel string, w http.ResponseWriter, r *http.Request) error {
	headers := map[string]string{}
	// Go exige un ID de sesión estable para enrutar y cachear prompts.
	// Claude Code envía el suyo en X-Claude-Code-Session-Id (Go lo reconoce).
	if sid := sessionID(r); sid != "" {
		headers["x-opencode-session"] = sid
	}
	if native := r.Header.Get("X-Claude-Code-Session-Id"); native != "" {
		headers["X-Claude-Code-Session-Id"] = native
	}

	switch preferredProtocol(targetModel) {
	case protoMessages:
		// Passthrough nativo: reenviamos el body Anthropic tal cual.
		if v := r.Header.Get("anthropic-version"); v != "" {
			headers["anthropic-version"] = v
		} else {
			headers["anthropic-version"] = "2023-06-01"
		}
		return c.chatMessages(ctx, req, targetModel, w, headers)
	case protoResponses:
		return c.chatResponses(ctx, req, targetModel, w, headers)
	default:
		return c.chatCompletions(ctx, req, targetModel, w, headers)
	}
}

// preferredProtocol indica el protocolo recomendado para cada modelo.
// Según https://opencode.ai/docs/go/#endpoints: algunos solo hablan
// /responses (grok, gpt-luna, muse-spark), otros /messages nativo
// (claude-haiku, minimax, qwen3.7/3.8) y el resto /chat/completions.
func preferredProtocol(model string) protocol {
	id := strings.ToLower(strings.TrimSpace(model))
	switch {
	case strings.Contains(id, "muse-spark"),
		strings.HasPrefix(id, "grok-4.6"), strings.HasPrefix(id, "grok-4.7"),
		strings.HasPrefix(id, "gpt-6-luna"), strings.HasPrefix(id, "gpt-5.6-luna"):
		return protoResponses
	case strings.HasPrefix(id, "claude-haiku-5-5"),
		strings.HasPrefix(id, "minimax-m"),
		strings.HasPrefix(id, "qwen3.7-plus"),
		strings.HasPrefix(id, "qwen3.8-max"), strings.HasPrefix(id, "qwen3.8-flash"):
		return protoMessages
	default:
		return protoChat
	}
}

// sessionID obtiene la sesión del cliente o genera una estable por petición.
func sessionID(r *http.Request) string {
	for _, h := range []string{"x-opencode-session", "X-Claude-Code-Session-Id", "X-Session-Id"} {
		if v := strings.TrimSpace(r.Header.Get(h)); v != "" {
			return v
		}
	}
	return newSessionID()
}

// newSessionID genera un ID aleatorio tipo UUID.
func newSessionID() string {
	var b [16]byte
	if _, err := rand.Read(b[:]); err != nil {
		return fmt.Sprintf("sess-%d", time.Now().UnixNano())
	}
	b[6] = (b[6] & 0x0f) | 0x40
	b[8] = (b[8] & 0x3f) | 0x80
	return fmt.Sprintf("%x-%x-%x-%x-%x", b[0:4], b[4:6], b[6:8], b[8:10], b[10:16])
}

// chatMessages reenvía la petición Anthropic tal cual al endpoint /messages
// nativo del proveedor (sin traducción). Es el camino preferido para Claude
// Code porque conserva tool_use, cache_control, thinking, etc.
func (c *Connector) chatMessages(ctx context.Context, req *anthropic.MessagesRequest, targetModel string, w http.ResponseWriter, headers map[string]string) error {
	payload := rewriteModel(ctx, req, targetModel)
	// El endpoint /messages nativo (Anthropic) autentica con x-api-key.
	if c.apiKey != "" {
		headers["x-api-key"] = c.apiKey
	}
	resp, err := connector.PostJSONHeaders(ctx, c.client, c.baseURL+"/messages", payload, headers)
	if err != nil {
		return fmt.Errorf("%s no alcanzable: %w", c.name, err)
	}
	defer resp.Body.Close()

	if ct := resp.Header.Get("Content-Type"); ct != "" {
		w.Header().Set("Content-Type", ct)
	} else {
		w.Header().Set("Content-Type", "application/json")
	}
	w.WriteHeader(resp.StatusCode)
	_, err = io.Copy(w, resp.Body)
	return err
}

// rewriteModel toma el body original (si existe) y sustituye solo el modelo.
func rewriteModel(ctx context.Context, req *anthropic.MessagesRequest, targetModel string) []byte {
	if raw := connector.RawBody(ctx); len(raw) > 0 {
		var m map[string]any
		if err := json.Unmarshal(raw, &m); err == nil {
			m["model"] = targetModel
			if b, err := json.Marshal(m); err == nil {
				return b
			}
		}
	}
	clone := *req
	clone.Model = targetModel
	b, _ := json.Marshal(&clone)
	return b
}

func (c *Connector) chatCompletions(ctx context.Context, req *anthropic.MessagesRequest, targetModel string, w http.ResponseWriter, headers map[string]string) error {
	chatReq, err := translate.AnthropicToOpenAI(req, targetModel)
	if err != nil {
		return err
	}
	payload, err := json.Marshal(chatReq)
	if err != nil {
		return fmt.Errorf("serializar petición %s: %w", c.name, err)
	}
	resp, err := connector.PostJSON(ctx, c.client, c.baseURL+"/chat/completions", c.apiKey, payload, headers)
	if err != nil {
		return fmt.Errorf("%s no alcanzable: %w", c.name, err)
	}
	defer resp.Body.Close()

	if req.Stream {
		if resp.StatusCode < 200 || resp.StatusCode >= 300 {
			return connector.WriteProviderError(w, resp, c.name)
		}
		connector.WriteSSEHeaders(w)
		return translate.OpenAIStreamToAnthropic(resp.Body, w, req.Model)
	}
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return connector.WriteProviderError(w, resp, c.name)
	}
	var chatResp oai.ChatResponse
	if err := json.NewDecoder(resp.Body).Decode(&chatResp); err != nil {
		return fmt.Errorf("decodificar respuesta %s: %w", c.name, err)
	}
	anthResp, err := translate.OpenAIToAnthropic(&chatResp, req.Model)
	if err != nil {
		return err
	}
	return connector.WriteJSON(w, http.StatusOK, anthResp)
}

func (c *Connector) chatResponses(ctx context.Context, req *anthropic.MessagesRequest, targetModel string, w http.ResponseWriter, headers map[string]string) error {
	respReq, err := translate.AnthropicToResponses(req, targetModel)
	if err != nil {
		return err
	}
	payload, err := json.Marshal(respReq)
	if err != nil {
		return fmt.Errorf("serializar petición %s: %w", c.name, err)
	}
	resp, err := connector.PostJSON(ctx, c.client, c.baseURL+"/responses", c.apiKey, payload, headers)
	if err != nil {
		return fmt.Errorf("%s no alcanzable: %w", c.name, err)
	}
	defer resp.Body.Close()

	if req.Stream {
		if resp.StatusCode < 200 || resp.StatusCode >= 300 {
			return connector.WriteProviderError(w, resp, c.name)
		}
		connector.WriteSSEHeaders(w)
		return translate.ResponsesStreamToAnthropic(resp.Body, w, req.Model)
	}
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return connector.WriteProviderError(w, resp, c.name)
	}
	var responsesResp responses.Response
	if err := json.NewDecoder(resp.Body).Decode(&responsesResp); err != nil {
		return fmt.Errorf("decodificar respuesta %s: %w", c.name, err)
	}
	anthResp, err := translate.ResponsesToAnthropic(&responsesResp, req.Model)
	if err != nil {
		return err
	}
	return connector.WriteJSON(w, http.StatusOK, anthResp)
}
