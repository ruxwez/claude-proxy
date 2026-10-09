// Package adapter define la interfaz que debe implementar cada proveedor
// (OpenAI, Anthropic, y futuros: Gemini, Ollama, etc.).
package adapter

import (
	"context"
	"fmt"
	"net/http"
	"sort"
	"sync"

	"github.com/ruxwez/claude-proxy/internal/anthropic"
	"github.com/ruxwez/claude-proxy/internal/config"
)

// Adapter traduce una petición Anthropic y escribe una respuesta Anthropic.
type Adapter interface {
	Provider() string
	Handle(ctx context.Context, anthReq *anthropic.MessagesRequest, route config.ResolvedRoute, w http.ResponseWriter, r *http.Request) error
}

var (
	mu       sync.RWMutex
	registry = map[string]Adapter{}
)

// Register añade un adaptador. Llamar desde init() de cada proveedor.
func Register(a Adapter) {
	mu.Lock()
	defer mu.Unlock()
	registry[a.Provider()] = a
}

// Get devuelve el adaptador para un provider o error.
func Get(provider string) (Adapter, error) {
	mu.RLock()
	defer mu.RUnlock()
	a, ok := registry[provider]
	if !ok {
		return nil, fmt.Errorf("provider %q no soportado (registrados: %s)", provider, List())
	}
	return a, nil
}

// List devuelve los providers registrados ordenados.
func List() string {
	mu.RLock()
	defer mu.RUnlock()
	keys := make([]string, 0, len(registry))
	for k := range registry {
		keys = append(keys, k)
	}
	sort.Strings(keys)
	out := ""
	for i, k := range keys {
		if i > 0 {
			out += ", "
		}
		out += k
	}
	if out == "" {
		return "(ninguno)"
	}
	return out
}
