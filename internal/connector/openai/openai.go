// Package openai implementa el conector genérico para cualquier proveedor
// compatible con la API OpenAI (OpenAI, Ollama, Groq, vLLM, LM Studio...).
//
// Se configura por entorno:
//
//	OPENAI_ENABLED=true
//	OPENAI_API_KEY=...
//	OPENAI_BASE_URL=https://api.openai.com/v1
//	OPENAI_MODELS=gpt-4o-mini,gpt-4o   (opcional; si falta se consulta /models)
package openai

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"sort"
	"strings"
	"time"

	"github.com/ruxwez/claude-proxy/internal/anthropic"
	"github.com/ruxwez/claude-proxy/internal/connector"
	oai "github.com/ruxwez/claude-proxy/internal/openai"
	"github.com/ruxwez/claude-proxy/internal/translate"
)

const modelCacheTTL = 5 * time.Minute

// Connector es el adaptador OpenAI genérico.
type Connector struct {
	baseURL      string
	apiKey       string
	staticModels []string
	client       *http.Client
	cache        *connector.ModelCache
}

// New crea el conector OpenAI genérico.
func New(apiKey, baseURL string, staticModels []string, timeout time.Duration) *Connector {
	if timeout <= 0 {
		timeout = 120 * time.Second
	}
	return &Connector{
		baseURL:      strings.TrimRight(baseURL, "/"),
		apiKey:       apiKey,
		staticModels: staticModels,
		client:       &http.Client{Timeout: timeout},
		cache:        connector.NewModelCache(modelCacheTTL),
	}
}

// Name devuelve el prefijo del adaptador.
func (c *Connector) Name() string { return "openai" }

// Models lista los modelos configurados o consulta {base_url}/models.
func (c *Connector) Models(ctx context.Context) ([]connector.Model, error) {
	if len(c.staticModels) > 0 {
		return c.fixedModels(), nil
	}
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

func (c *Connector) fixedModels() []connector.Model {
	out := make([]connector.Model, 0, len(c.staticModels))
	for _, id := range c.staticModels {
		out = append(out, connector.Model{ID: id, DisplayName: id})
	}
	sort.Slice(out, func(i, j int) bool { return out[i].ID < out[j].ID })
	return out
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
		return nil, fmt.Errorf("openai: no se pudo listar modelos: %w", err)
	}
	defer resp.Body.Close()
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return nil, fmt.Errorf("openai: /models devolvió %s", resp.Status)
	}

	var parsed struct {
		Data []struct {
			ID      string `json:"id"`
			Created int64  `json:"created"`
		} `json:"data"`
	}
	if err := json.NewDecoder(resp.Body).Decode(&parsed); err != nil {
		return nil, fmt.Errorf("openai: respuesta /models inválida: %w", err)
	}
	out := make([]connector.Model, 0, len(parsed.Data))
	for _, m := range parsed.Data {
		if id := strings.TrimSpace(m.ID); id != "" {
			out = append(out, connector.Model{ID: id, DisplayName: id, Created: m.Created})
		}
	}
	sort.Slice(out, func(i, j int) bool { return out[i].ID < out[j].ID })
	return out, nil
}

// Chat traduce la petición Anthropic a Chat Completions y ejecuta.
func (c *Connector) Chat(ctx context.Context, req *anthropic.MessagesRequest, targetModel string, w http.ResponseWriter, r *http.Request) error {
	chatReq, err := translate.AnthropicToOpenAI(req, targetModel)
	if err != nil {
		return err
	}
	payload, err := json.Marshal(chatReq)
	if err != nil {
		return fmt.Errorf("serializar petición openai: %w", err)
	}
	headers := map[string]string{}
	resp, err := connector.PostJSON(ctx, c.client, c.baseURL+"/chat/completions", c.apiKey, payload, headers)
	if err != nil {
		return fmt.Errorf("openai no alcanzable: %w", err)
	}
	defer resp.Body.Close()

	if req.Stream {
		if resp.StatusCode < 200 || resp.StatusCode >= 300 {
			return connector.WriteProviderError(w, resp, "openai")
		}
		connector.WriteSSEHeaders(w)
		return translate.OpenAIStreamToAnthropic(resp.Body, w, req.Model)
	}
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return connector.WriteProviderError(w, resp, "openai")
	}
	var chatResp oai.ChatResponse
	if err := json.NewDecoder(resp.Body).Decode(&chatResp); err != nil {
		return fmt.Errorf("decodificar respuesta openai: %w", err)
	}
	anthResp, err := translate.OpenAIToAnthropic(&chatResp, req.Model)
	if err != nil {
		return err
	}
	return connector.WriteJSON(w, http.StatusOK, anthResp)
}
