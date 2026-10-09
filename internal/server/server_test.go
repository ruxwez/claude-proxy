package server

import (
	"bytes"
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/ruxwez/claude-proxy/internal/config"
)

// mockOpenAI simula /chat/completions (no-stream y stream).
func mockOpenAI(t *testing.T) *httptest.Server {
	t.Helper()
	return httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		var req map[string]any
		_ = json.NewDecoder(r.Body).Decode(&req)
		if auth := r.Header.Get("authorization"); auth != "Bearer sk-mock" {
			w.WriteHeader(http.StatusUnauthorized)
			_, _ = w.Write([]byte(`{"error":{"message":"bad key","type":"auth"}}`))
			return
		}
		if req["model"] != "gpt-4o-mini" {
			w.WriteHeader(http.StatusBadRequest)
			_, _ = w.Write([]byte(`{"error":{"message":"bad model","type":"invalid"}}`))
			return
		}
		stream, _ := req["stream"].(bool)
		if stream {
			w.Header().Set("Content-Type", "text/event-stream")
			w.WriteHeader(http.StatusOK)
			fmt.Fprintln(w, `data: {"id":"chatcmpl-1","object":"chat.completion.chunk","created":1,"model":"gpt-4o-mini","choices":[{"index":0,"delta":{"role":"assistant","content":"Hola"},"finish_reason":null}]}`)
			fmt.Fprintln(w, `data: {"id":"chatcmpl-1","object":"chat.completion.chunk","created":1,"model":"gpt-4o-mini","choices":[{"index":0,"delta":{"content":" mock"},"finish_reason":null}]}`)
			fmt.Fprintln(w, `data: {"id":"chatcmpl-1","object":"chat.completion.chunk","created":1,"model":"gpt-4o-mini","choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":7,"completion_tokens":4,"total_tokens":11}}`)
			fmt.Fprintln(w, `data: [DONE]`)
			return
		}
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(`{
			"id": "chatcmpl-1", "object": "chat.completion", "created": 1, "model": "gpt-4o-mini",
			"choices": [{"index": 0, "message": {"role": "assistant", "content": "Hola mock"}, "finish_reason": "stop"}],
			"usage": {"prompt_tokens": 7, "completion_tokens": 4, "total_tokens": 11}
		}`))
	}))
}

func testServer(t *testing.T, mockURL string) *Server {
	t.Helper()
	cfg := &config.Config{
		Server: config.ServerConfig{Port: 0},
		Routes: []config.Route{{
			AnthropicModel: "claude-3-5-sonnet-20241022",
			Provider:       "openai",
			TargetModel:    "gpt-4o-mini",
			BaseURL:        mockURL,
			APIKey:         "sk-mock",
			TimeoutSeconds: 10,
		}},
	}
	return New(cfg, nil)
}

func TestE2E_NonStream(t *testing.T) {
	mock := mockOpenAI(t)
	defer mock.Close()
	srv := httptest.NewServer(testServer(t, mock.URL))
	defer srv.Close()

	body := `{"model":"claude-3-5-sonnet-20241022","max_tokens":64,"messages":[{"role":"user","content":"Hola"}]}`
	resp, err := http.Post(srv.URL+"/v1/messages", "application/json", strings.NewReader(body))
	if err != nil {
		t.Fatal(err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != 200 {
		t.Fatalf("status = %d", resp.StatusCode)
	}
	var out map[string]any
	if err := json.NewDecoder(resp.Body).Decode(&out); err != nil {
		t.Fatal(err)
	}
	if out["type"] != "message" || out["model"] != "claude-3-5-sonnet-20241022" {
		t.Fatalf("respuesta no Anthropic: %v", out)
	}
	content, _ := json.Marshal(out["content"])
	if !bytes.Contains(content, []byte("Hola mock")) {
		t.Fatalf("content sin texto del mock: %s", content)
	}
}

func TestE2E_Stream(t *testing.T) {
	mock := mockOpenAI(t)
	defer mock.Close()
	srv := httptest.NewServer(testServer(t, mock.URL))
	defer srv.Close()

	body := `{"model":"claude-3-5-sonnet-20241022","max_tokens":64,"stream":true,"messages":[{"role":"user","content":"Hola"}]}`
	resp, err := http.Post(srv.URL+"/v1/messages", "application/json", strings.NewReader(body))
	if err != nil {
		t.Fatal(err)
	}
	defer resp.Body.Close()
	if ct := resp.Header.Get("Content-Type"); ct != "text/event-stream" {
		t.Fatalf("content-type = %q", ct)
	}
	buf := new(bytes.Buffer)
	_, _ = buf.ReadFrom(resp.Body)
	s := buf.String()
	for _, want := range []string{"message_start", "content_block_delta", "message_stop", "Hola"} {
		if !strings.Contains(s, want) {
			t.Fatalf("stream sin %q:\n%s", want, s)
		}
	}
}

func TestE2E_ModelNotConfigured(t *testing.T) {
	mock := mockOpenAI(t)
	defer mock.Close()
	srv := httptest.NewServer(testServer(t, mock.URL))
	defer srv.Close()

	body := `{"model":"no-existe","max_tokens":10,"messages":[{"role":"user","content":"hi"}]}`
	resp, err := http.Post(srv.URL+"/v1/messages", "application/json", strings.NewReader(body))
	if err != nil {
		t.Fatal(err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusNotFound {
		t.Fatalf("status = %d, want 404", resp.StatusCode)
	}
}

func TestE2E_HealthModelsCountTokens(t *testing.T) {
	mock := mockOpenAI(t)
	defer mock.Close()
	s := testServer(t, mock.URL)
	ts := httptest.NewServer(s)
	defer ts.Close()

	if r, _ := http.Get(ts.URL + "/health"); r.StatusCode != 200 {
		t.Fatalf("health = %d", r.StatusCode)
		r.Body.Close()
	} else {
		r.Body.Close()
	}
	r, err := http.Get(ts.URL + "/v1/models")
	if err != nil {
		t.Fatal(err)
	}
	var models map[string]any
	_ = json.NewDecoder(r.Body).Decode(&models)
	r.Body.Close()
	if !strings.Contains(fmt.Sprint(models), "claude-3-5-sonnet-20241022") {
		t.Fatalf("models sin el modelo configurado: %v", models)
	}

	ctBody := `{"model":"claude-3-5-sonnet-20241022","messages":[{"role":"user","content":"hola mundo"}]}`
	r2, err := http.Post(ts.URL+"/v1/messages/count_tokens", "application/json", strings.NewReader(ctBody))
	if err != nil {
		t.Fatal(err)
	}
	var ct map[string]any
	_ = json.NewDecoder(r2.Body).Decode(&ct)
	r2.Body.Close()
	if _, ok := ct["input_tokens"]; !ok {
		t.Fatalf("count_tokens sin input_tokens: %v", ct)
	}
}
