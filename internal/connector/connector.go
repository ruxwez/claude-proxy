// Package connector define el contrato que cumple cada proveedor al que el
// proxy traduce peticiones Anthropic (OpenCode Zen, OpenCode Go, OpenAI...).
//
// Cada conector sabe listar sus modelos (con su ID real) y atender un
// POST /v1/messages traduciéndolo al estándar del proveedor.
package connector

import (
	"bytes"
	"context"
	"encoding/json"
	"io"
	"net/http"
	"sort"
	"strings"
	"sync"
	"time"

	"github.com/ruxwez/claude-proxy/internal/anthropic"
)

// Model es un modelo ofrecido por un conector.
type Model struct {
	ID          string // ID real en el proveedor (sin prefijo)
	DisplayName string
	Created     int64 // unix seconds (0 si desconocido)
}

// Connector traduce peticiones Anthropic hacia un proveedor concreto.
//
// El servidor identifica cada conector con un Name() (p. ej. "opencode-zen")
// y expone sus modelos como "Name()/ID".
type Connector interface {
	// Name es el prefijo del adaptador (p. ej. "opencode-zen").
	Name() string
	// Models lista los modelos disponibles en el proveedor.
	Models(ctx context.Context) ([]Model, error)
	// Chat traduce y ejecuta una petición de mensajes contra targetModel
	// (el ID real dentro del conector) escribiendo la respuesta Anthropic en w.
	Chat(ctx context.Context, req *anthropic.MessagesRequest, targetModel string, w http.ResponseWriter, r *http.Request) error
}

// Registry guarda los conectores activos.
type Registry struct {
	mu     sync.RWMutex
	byName map[string]Connector
	order  []string
}

// NewRegistry crea un registro vacío.
func NewRegistry() *Registry {
	return &Registry{byName: map[string]Connector{}}
}

// Add registra un conector. Ignora nil.
func (r *Registry) Add(c Connector) {
	if c == nil {
		return
	}
	r.mu.Lock()
	defer r.mu.Unlock()
	name := c.Name()
	if _, exists := r.byName[name]; !exists {
		r.order = append(r.order, name)
	}
	r.byName[name] = c
}

// Get devuelve el conector por nombre.
func (r *Registry) Get(name string) (Connector, bool) {
	r.mu.RLock()
	defer r.mu.RUnlock()
	c, ok := r.byName[name]
	return c, ok
}

// Connectors devuelve los conectores en orden de registro.
func (r *Registry) Connectors() []Connector {
	r.mu.RLock()
	defer r.mu.RUnlock()
	out := make([]Connector, 0, len(r.order))
	for _, name := range r.order {
		out = append(out, r.byName[name])
	}
	return out
}

// Names devuelve los nombres de los conectores ordenados alfabéticamente.
func (r *Registry) Names() []string {
	r.mu.RLock()
	defer r.mu.RUnlock()
	out := make([]string, 0, len(r.byName))
	for name := range r.byName {
		out = append(out, name)
	}
	sort.Strings(out)
	return out
}

// PostJSON envía un POST JSON con auth Bearer y cabeceras extra.
//
// Usa http.Client con Timeout (no un context con deadline) para no abortar
// el stream mientras el llamante sigue leyendo resp.Body.
func PostJSON(ctx context.Context, client *http.Client, url, apiKey string, payload []byte, headers map[string]string) (*http.Response, error) {
	h := make(map[string]string, len(headers)+1)
	for k, v := range headers {
		h[k] = v
	}
	if apiKey != "" {
		h["Authorization"] = "Bearer " + apiKey
	}
	return PostJSONHeaders(ctx, client, url, payload, h)
}

// PostJSONHeaders envía un POST JSON con exactamente las cabeceras dadas
// (sin añadir auth). Útil para endpoints que esperan x-api-key.
func PostJSONHeaders(ctx context.Context, client *http.Client, url string, payload []byte, headers map[string]string) (*http.Response, error) {
	httpReq, err := http.NewRequestWithContext(ctx, http.MethodPost, url, bytes.NewReader(payload))
	if err != nil {
		return nil, err
	}
	httpReq.Header.Set("Content-Type", "application/json")
	if httpReq.Header.Get("User-Agent") == "" {
		httpReq.Header.Set("User-Agent", "claude-proxy/1.0")
	}
	for k, v := range headers {
		httpReq.Header.Set(k, v)
	}
	return client.Do(httpReq)
}

// WriteSSEHeaders prepara la respuesta para Server-Sent Events.
func WriteSSEHeaders(w http.ResponseWriter) {
	h := w.Header()
	h.Set("Content-Type", "text/event-stream")
	h.Set("Cache-Control", "no-cache")
	h.Set("Connection", "keep-alive")
	h.Set("X-Accel-Buffering", "no")
	w.WriteHeader(http.StatusOK)
}

// WriteProviderError reenvía un error del proveedor en formato Anthropic
// y devuelve nil (ya se escribió la respuesta).
func WriteProviderError(w http.ResponseWriter, resp *http.Response, provider string) error {
	raw, _ := io.ReadAll(io.LimitReader(resp.Body, 1<<20))
	msg := strings.TrimSpace(string(raw))
	if msg == "" {
		msg = "error del proveedor " + provider + ": status " + resp.Status
	} else {
		var parsed struct {
			Error struct {
				Message string `json:"message"`
			} `json:"error"`
		}
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

// WriteJSON escribe v como JSON con el status indicado.
func WriteJSON(w http.ResponseWriter, status int, v any) error {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	return json.NewEncoder(w).Encode(v)
}

// rawBodyKey es la clave de contexto del body original de la petición.
type rawBodyKey struct{}

// ContextWithRaw guarda el body original para el passthrough nativo Anthropic.
func ContextWithRaw(ctx context.Context, raw []byte) context.Context {
	return context.WithValue(ctx, rawBodyKey{}, raw)
}

// RawBody recupera el body original de la petición, o nil.
func RawBody(ctx context.Context) []byte {
	b, _ := ctx.Value(rawBodyKey{}).([]byte)
	return b
}

// ModelCache es una caché con TTL para listas de modelos.
type ModelCache struct {
	mu     sync.Mutex
	models []Model
	at     time.Time
	ttl    time.Duration
}

// NewModelCache crea una caché con el TTL indicado (def. 5 min si es <= 0).
func NewModelCache(ttl time.Duration) *ModelCache {
	if ttl <= 0 {
		ttl = 5 * time.Minute
	}
	return &ModelCache{ttl: ttl}
}

// Get devuelve los modelos cacheados si siguen vigentes.
func (c *ModelCache) Get() ([]Model, bool) {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.models == nil || time.Since(c.at) > c.ttl {
		return nil, false
	}
	return c.models, true
}

// Set guarda una lista de modelos.
func (c *ModelCache) Set(models []Model) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.models = models
	c.at = time.Now()
}
