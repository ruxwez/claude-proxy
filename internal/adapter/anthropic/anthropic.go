// Package anthropic implementa el passthrough al API real de Anthropic.
// Útil cuando algunas rutas del .toml deben ir directas sin traducción.
package anthropic

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"time"

	"github.com/ruxwez/claude-proxy/internal/adapter"
	anthropictypes "github.com/ruxwez/claude-proxy/internal/anthropic"
	"github.com/ruxwez/claude-proxy/internal/config"
)

func init() {
	adapter.Register(&Adapter{})
}

// Adapter reenvía la petición tal cual a {base_url}/messages.
type Adapter struct{}

func (a *Adapter) Provider() string { return "anthropic" }

func (a *Adapter) Handle(ctx context.Context, anthReq *anthropictypes.MessagesRequest, route config.ResolvedRoute, w http.ResponseWriter, r *http.Request) error {
	timeout := time.Duration(route.TimeoutSeconds) * time.Second
	if timeout <= 0 {
		timeout = 120 * time.Second
	}
	ctx, cancel := context.WithTimeout(ctx, timeout)
	defer cancel()

	raw, ok := ctx.Value(rawKey{}).([]byte)
	if !ok || len(raw) == 0 {
		return fmt.Errorf("passthrough anthropic requiere body original")
	}

	// Sustituir "model" por el target manteniendo el resto de campos futuros.
	var m map[string]any
	if err := json.Unmarshal(raw, &m); err != nil {
		return fmt.Errorf("body inválido: %w", err)
	}
	m["model"] = route.TargetModel
	fwd, err := json.Marshal(m)
	if err != nil {
		return err
	}

	url := route.BaseURL + "/messages"
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, url, bytes.NewReader(fwd))
	if err != nil {
		return err
	}
	req.Header.Set("Content-Type", "application/json")
	if route.APIKey != "" {
		req.Header.Set("x-api-key", route.APIKey)
	}
	if v := r.Header.Get("anthropic-version"); v != "" {
		req.Header.Set("anthropic-version", v)
	} else {
		req.Header.Set("anthropic-version", "2023-06-01")
	}
	if v := r.Header.Get("anthropic-beta"); v != "" {
		req.Header.Set("anthropic-beta", v)
	}
	for k, v := range route.ExtraHeaders {
		req.Header.Set(k, v)
	}

	resp, err := (&http.Client{}).Do(req)
	if err != nil {
		return fmt.Errorf("proveedor anthropic no alcanzable: %w", err)
	}
	defer resp.Body.Close()

	for _, h := range []string{"Content-Type"} {
		if v := resp.Header.Get(h); v != "" {
			w.Header().Set(h, v)
		}
	}
	w.WriteHeader(resp.StatusCode)
	_, err = io.Copy(w, io.LimitReader(resp.Body, 32<<20))
	return err
}

type rawKey struct{}

// ContextWithRaw guarda el body original para el passthrough.
func ContextWithRaw(ctx context.Context, raw []byte) context.Context {
	return context.WithValue(ctx, rawKey{}, raw)
}
