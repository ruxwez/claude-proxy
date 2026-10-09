package server

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/ruxwez/claude-proxy/internal/anthropic"
	"github.com/ruxwez/claude-proxy/internal/config"
	"github.com/ruxwez/claude-proxy/internal/connector"
)

// fakeConnector implementa connector.Connector para tests.
type fakeConnector struct {
	chatModel string
}

func (fakeConnector) Name() string { return "fake" }

func (fakeConnector) Models(context.Context) ([]connector.Model, error) {
	return []connector.Model{
		{ID: "beta", DisplayName: "Beta"},
		{ID: "alpha-free", DisplayName: "Alpha"},
	}, nil
}

func (f *fakeConnector) Chat(_ context.Context, req *anthropic.MessagesRequest, targetModel string, w http.ResponseWriter, _ *http.Request) error {
	f.chatModel = targetModel
	return connector.WriteJSON(w, http.StatusOK, &anthropic.MessagesResponse{
		ID:    "msg_test",
		Type:  "message",
		Role:  "assistant",
		Model: req.Model,
		Content: []any{
			map[string]any{"type": "text", "text": "target=" + targetModel},
		},
	})
}

func newTestServer(t *testing.T, authToken string) *Server {
	t.Helper()
	reg := connector.NewRegistry()
	reg.Add(&fakeConnector{})
	cfg := config.Settings{Host: "127.0.0.1", Port: 0, AuthToken: authToken}
	return New(cfg, reg, nil)
}

func TestModelsArePrefixed(t *testing.T) {
	srv := newTestServer(t, "")
	req := httptest.NewRequest(http.MethodGet, "/v1/models", nil)
	rec := httptest.NewRecorder()
	srv.ServeHTTP(rec, req)

	if rec.Code != http.StatusOK {
		t.Fatalf("status = %d, want 200", rec.Code)
	}
	var resp anthropic.ModelsResponse
	if err := json.Unmarshal(rec.Body.Bytes(), &resp); err != nil {
		t.Fatalf("json: %v", err)
	}
	if len(resp.Data) != 2 {
		t.Fatalf("modelos = %d, want 2", len(resp.Data))
	}
	// Orden alfabético por ID completo.
	if resp.Data[0].ID != "fake/alpha-free" || resp.Data[1].ID != "fake/beta" {
		t.Fatalf("IDs inesperados: %s, %s", resp.Data[0].ID, resp.Data[1].ID)
	}
	if resp.Data[0].Type != "model" || resp.Data[0].DisplayName == "" {
		t.Fatalf("campos incompletos: %+v", resp.Data[0])
	}
	if resp.HasMore {
		t.Fatalf("has_more = true, want false")
	}
}

func TestMessagesRoutesByPrefix(t *testing.T) {
	srv := newTestServer(t, "")
	body := `{"model":"fake/beta","max_tokens":16,"messages":[{"role":"user","content":"hola"}]}`
	req := httptest.NewRequest(http.MethodPost, "/v1/messages", strings.NewReader(body))
	req.Header.Set("content-type", "application/json")
	rec := httptest.NewRecorder()
	srv.ServeHTTP(rec, req)

	if rec.Code != http.StatusOK {
		t.Fatalf("status = %d (%s)", rec.Code, rec.Body.String())
	}
	var resp anthropic.MessagesResponse
	if err := json.Unmarshal(rec.Body.Bytes(), &resp); err != nil {
		t.Fatalf("json: %v", err)
	}
	if resp.Model != "fake/beta" {
		t.Fatalf("model = %q, want fake/beta", resp.Model)
	}
	if len(resp.Content) == 0 {
		t.Fatalf("respuesta sin contenido")
	}
}

func TestMessagesRequiresPrefix(t *testing.T) {
	srv := newTestServer(t, "")
	body := `{"model":"beta","max_tokens":16,"messages":[{"role":"user","content":"hola"}]}`
	req := httptest.NewRequest(http.MethodPost, "/v1/messages", strings.NewReader(body))
	req.Header.Set("content-type", "application/json")
	rec := httptest.NewRecorder()
	srv.ServeHTTP(rec, req)

	if rec.Code != http.StatusNotFound {
		t.Fatalf("status = %d, want 404", rec.Code)
	}
}

func TestMessagesUnknownConnector(t *testing.T) {
	srv := newTestServer(t, "")
	body := `{"model":"nope/beta","max_tokens":16,"messages":[{"role":"user","content":"hola"}]}`
	req := httptest.NewRequest(http.MethodPost, "/v1/messages", strings.NewReader(body))
	req.Header.Set("content-type", "application/json")
	rec := httptest.NewRecorder()
	srv.ServeHTTP(rec, req)

	if rec.Code != http.StatusNotFound {
		t.Fatalf("status = %d, want 404", rec.Code)
	}
}

func TestAuthToken(t *testing.T) {
	srv := newTestServer(t, "secreto")

	// Sin token -> 401.
	req := httptest.NewRequest(http.MethodGet, "/v1/models", nil)
	rec := httptest.NewRecorder()
	srv.ServeHTTP(rec, req)
	if rec.Code != http.StatusUnauthorized {
		t.Fatalf("sin token status = %d, want 401", rec.Code)
	}

	// Con x-api-key -> 200.
	req = httptest.NewRequest(http.MethodGet, "/v1/models", nil)
	req.Header.Set("x-api-key", "secreto")
	rec = httptest.NewRecorder()
	srv.ServeHTTP(rec, req)
	if rec.Code != http.StatusOK {
		t.Fatalf("con token status = %d, want 200", rec.Code)
	}
}

func TestSplitModel(t *testing.T) {
	cases := []struct {
		in   string
		name string
		raw  string
		ok   bool
	}{
		{"opencode-zen/exo-free", "opencode-zen", "exo-free", true},
		{"openai/gpt-4o", "openai", "gpt-4o", true},
		{"beta", "", "", false},
		{"fake/", "", "", false},
		{"/beta", "", "", false},
	}
	for _, c := range cases {
		name, raw, ok := splitModel(c.in)
		if ok != c.ok || name != c.name || raw != c.raw {
			t.Errorf("splitModel(%q) = (%q,%q,%v), want (%q,%q,%v)", c.in, name, raw, ok, c.name, c.raw, c.ok)
		}
	}
}
