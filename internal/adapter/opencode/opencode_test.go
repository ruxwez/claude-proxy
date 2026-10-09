package opencode_test

import (
	"bytes"
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/ruxwez/claude-proxy/internal/config"
	"github.com/ruxwez/claude-proxy/internal/server"
)

func testRoute(provider, target, base string) config.ResolvedRoute {
	return config.ResolvedRoute{
		Provider:       provider,
		TargetModel:    target,
		BaseURL:        base,
		APIKey:         "sk-zen-test",
		TimeoutSeconds: 10,
	}
}

// mockZenChat simula POST /chat/completions de Zen.
func mockZenChat(t *testing.T, check func(r *http.Request, body map[string]any)) *httptest.Server {
	t.Helper()
	return httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/chat/completions" {
			w.WriteHeader(http.StatusNotFound)
			return
		}
		if r.Header.Get("authorization") != "Bearer sk-zen-test" {
			w.WriteHeader(http.StatusUnauthorized)
			_, _ = w.Write([]byte(`{"error":{"message":"bad key","type":"auth"}}`))
			return
		}
		if ua := r.Header.Get("User-Agent"); ua == "" {
			w.WriteHeader(http.StatusBadRequest)
			_, _ = w.Write([]byte(`{"error":{"message":"user-agent requerido","type":"invalid"}}`))
			return
		}
		var body map[string]any
		_ = json.NewDecoder(r.Body).Decode(&body)
		if check != nil {
			check(r, body)
		}
		stream, _ := body["stream"].(bool)
		if stream {
			w.Header().Set("Content-Type", "text/event-stream")
			w.WriteHeader(http.StatusOK)
			fmt.Fprintln(w, `data: {"id":"chatcmpl-1","object":"chat.completion.chunk","created":1,"model":"exo-free","choices":[{"index":0,"delta":{"role":"assistant","content":"Hola"},"finish_reason":null}]}`)
			fmt.Fprintln(w, `data: {"id":"chatcmpl-1","object":"chat.completion.chunk","created":1,"model":"exo-free","choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":6,"completion_tokens":3,"total_tokens":9}}`)
			fmt.Fprintln(w, `data: [DONE]`)
			return
		}
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(`{
			"id":"chatcmpl-1","object":"chat.completion","created":1,"model":"exo-free",
			"choices":[{"index":0,"message":{"role":"assistant","content":"Hola zen"},"finish_reason":"stop"}],
			"usage":{"prompt_tokens":6,"completion_tokens":3,"total_tokens":9}
		}`))
	}))
}

// mockZenResponses simula POST /responses de Zen.
func mockZenResponses(t *testing.T) *httptest.Server {
	t.Helper()
	return httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/responses" {
			w.WriteHeader(http.StatusNotFound)
			return
		}
		if r.Header.Get("authorization") != "Bearer sk-zen-test" {
			w.WriteHeader(http.StatusUnauthorized)
			_, _ = w.Write([]byte(`{"error":{"message":"bad key"}}`))
			return
		}
		var body map[string]any
		_ = json.NewDecoder(r.Body).Decode(&body)
		if body["model"] != "muse-spark-1.3-contributor-free" {
			w.WriteHeader(http.StatusBadRequest)
			_, _ = w.Write([]byte(`{"error":{"message":"bad model"}}`))
			return
		}
		if _, ok := body["max_output_tokens"]; !ok {
			w.WriteHeader(http.StatusBadRequest)
			_, _ = w.Write([]byte(`{"error":{"message":"max_output_tokens requerido"}}`))
			return
		}
		stream, _ := body["stream"].(bool)
		if stream {
			w.Header().Set("Content-Type", "text/event-stream")
			w.WriteHeader(http.StatusOK)
			fmt.Fprintln(w, `data: {"type":"response.output_text.delta","output_index":0,"delta":"Hola"}`)
			fmt.Fprintln(w, `data: {"type":"response.completed","response":{"id":"resp_1","status":"completed","usage":{"input_tokens":6,"output_tokens":3,"total_tokens":9}}}`)
			fmt.Fprintln(w, `data: [DONE]`)
			return
		}
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(`{
			"id":"resp_1","model":"muse-spark-1.3-contributor-free","status":"completed",
			"output":[{"type":"message","role":"assistant","status":"completed",
				"content":[{"type":"output_text","text":"Hola spark"}]}],
			"usage":{"input_tokens":6,"output_tokens":3,"total_tokens":9}
		}`))
	}))
}

func proxyWith(routes []config.Route) *server.Server {
	return server.New(&config.Config{Routes: routes}, nil)
}

func TestE2E_OpencodeChat_NonStream(t *testing.T) {
	var gotModel string
	var gotSession string
	mock := mockZenChat(t, func(r *http.Request, body map[string]any) {
		gotModel, _ = body["model"].(string)
		gotSession = r.Header.Get("x-opencode-session")
	})
	defer mock.Close()

	proxy := httptest.NewServer(proxyWith([]config.Route{{
		AnthropicModel: "exo-free", Provider: "opencode",
		TargetModel: "exo-free", BaseURL: mock.URL,
		APIKey: "sk-zen-test", TimeoutSeconds: 10,
	}}))
	defer proxy.Close()

	req, _ := http.NewRequest(http.MethodPost, proxy.URL+"/v1/messages",
		strings.NewReader(`{"model":"exo-free","max_tokens":64,"messages":[{"role":"user","content":"Hola"}]}`))
	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("x-opencode-session", "sess-123")
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatal(err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != 200 {
		t.Fatalf("status = %d", resp.StatusCode)
	}
	if gotModel != "exo-free" {
		t.Fatalf("model reenviado = %q", gotModel)
	}
	if gotSession != "sess-123" {
		t.Fatalf("x-opencode-session no propagada: %q", gotSession)
	}
	var out map[string]any
	if err := json.NewDecoder(resp.Body).Decode(&out); err != nil {
		t.Fatal(err)
	}
	if out["model"] != "exo-free" {
		t.Fatalf("model respuesta = %v", out["model"])
	}
	content, _ := json.Marshal(out["content"])
	if !bytes.Contains(content, []byte("Hola zen")) {
		t.Fatalf("content: %s", content)
	}
}

func TestE2E_OpencodeChat_Stream(t *testing.T) {
	mock := mockZenChat(t, nil)
	defer mock.Close()
	proxy := httptest.NewServer(proxyWith([]config.Route{{
		AnthropicModel: "exo-free", Provider: "opencode",
		TargetModel: "exo-free", BaseURL: mock.URL,
		APIKey: "sk-zen-test", TimeoutSeconds: 10,
	}}))
	defer proxy.Close()

	resp, err := http.Post(proxy.URL+"/v1/messages", "application/json",
		strings.NewReader(`{"model":"exo-free","max_tokens":64,"stream":true,"messages":[{"role":"user","content":"Hola"}]}`))
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

func TestE2E_OpencodeResponses_NonStream(t *testing.T) {
	mock := mockZenResponses(t)
	defer mock.Close()
	proxy := httptest.NewServer(proxyWith([]config.Route{{
		AnthropicModel: "muse-spark-1.3-contributor-free", Provider: "opencode-responses",
		TargetModel: "muse-spark-1.3-contributor-free", BaseURL: mock.URL,
		APIKey: "sk-zen-test", TimeoutSeconds: 10,
	}}))
	defer proxy.Close()

	resp, err := http.Post(proxy.URL+"/v1/messages", "application/json",
		strings.NewReader(`{"model":"muse-spark-1.3-contributor-free","max_tokens":64,"messages":[{"role":"user","content":"Hola"}]}`))
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
	content, _ := json.Marshal(out["content"])
	if !bytes.Contains(content, []byte("Hola spark")) {
		t.Fatalf("content: %s", content)
	}
}

func TestE2E_OpencodeResponses_Stream(t *testing.T) {
	mock := mockZenResponses(t)
	defer mock.Close()
	proxy := httptest.NewServer(proxyWith([]config.Route{{
		AnthropicModel: "muse-spark-1.3-contributor-free", Provider: "opencode-responses",
		TargetModel: "muse-spark-1.3-contributor-free", BaseURL: mock.URL,
		APIKey: "sk-zen-test", TimeoutSeconds: 10,
	}}))
	defer proxy.Close()

	resp, err := http.Post(proxy.URL+"/v1/messages", "application/json",
		strings.NewReader(`{"model":"muse-spark-1.3-contributor-free","max_tokens":64,"stream":true,"messages":[{"role":"user","content":"Hola"}]}`))
	if err != nil {
		t.Fatal(err)
	}
	defer resp.Body.Close()
	buf := new(bytes.Buffer)
	_, _ = buf.ReadFrom(resp.Body)
	s := buf.String()
	for _, want := range []string{"message_start", "Hola", "message_stop"} {
		if !strings.Contains(s, want) {
			t.Fatalf("stream sin %q:\n%s", want, s)
		}
	}
}

func TestE2E_OpencodeWildcardPassthrough(t *testing.T) {
	mock := mockZenChat(t, func(_ *http.Request, body map[string]any) {
		if body["model"] != "futuro-free" {
			t.Errorf("model = %v, want futuro-free", body["model"])
		}
	})
	defer mock.Close()
	proxy := httptest.NewServer(proxyWith([]config.Route{{
		AnthropicModel: "*-free", Provider: "opencode",
		TargetModel: "*", BaseURL: mock.URL,
		APIKey: "sk-zen-test", TimeoutSeconds: 10,
	}}))
	defer proxy.Close()

	resp, err := http.Post(proxy.URL+"/v1/messages", "application/json",
		strings.NewReader(`{"model":"futuro-free","max_tokens":10,"messages":[{"role":"user","content":"hi"}]}`))
	if err != nil {
		t.Fatal(err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != 200 {
		t.Fatalf("status = %d", resp.StatusCode)
	}
}
