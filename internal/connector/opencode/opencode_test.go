package opencode

import (
	"net/http/httptest"
	"testing"
)

func TestPreferredProtocol(t *testing.T) {
	cases := map[string]protocol{
		// Responses API.
		"muse-spark-1.3-contributor-free": protoResponses,
		"muse-spark-1.2-contributor":      protoResponses,
		"grok-4.7":                        protoResponses,
		"grok-4.6":                        protoResponses,
		"gpt-6-luna":                      protoResponses,
		"gpt-5.6-luna":                    protoResponses,
		// Anthropic Messages nativo.
		"claude-haiku-5-5": protoMessages,
		"minimax-m3":       protoMessages,
		"minimax-m2.7":     protoMessages,
		"qwen3.8-max":      protoMessages,
		"qwen3.8-flash":    protoMessages,
		"qwen3.7-plus":     protoMessages,
		// Chat Completions.
		"glm-5.3":                  protoChat,
		"glm-5.3-flash":            protoChat,
		"kimi-k3":                  protoChat,
		"deepseek-v4-pro":          protoChat,
		"longcat-2.5-preview-free": protoChat,
	}
	for model, want := range cases {
		if got := preferredProtocol(model); got != want {
			t.Errorf("preferredProtocol(%q) = %q, want %q", model, got, want)
		}
	}
}

func TestSessionIDFromHeaders(t *testing.T) {
	r := httptest.NewRequest("POST", "/v1/messages", nil)
	r.Header.Set("X-Claude-Code-Session-Id", "sesion-claude")
	if got := sessionID(r); got != "sesion-claude" {
		t.Fatalf("sessionID = %q, want sesion-claude", got)
	}

	r2 := httptest.NewRequest("POST", "/v1/messages", nil)
	r2.Header.Set("x-opencode-session", "sesion-opencode")
	if got := sessionID(r2); got != "sesion-opencode" {
		t.Fatalf("sessionID = %q, want sesion-opencode", got)
	}

	// Sin cabeceras: genera una sesión no vacía y estable dentro de la llamada.
	r3 := httptest.NewRequest("POST", "/v1/messages", nil)
	got := sessionID(r3)
	if got == "" {
		t.Fatalf("sessionID vacío sin cabeceras")
	}
	if again := sessionID(r3); again == got {
		// No es un error, pero documenta que se genera una nueva cada vez.
		t.Logf("sesión generada distinta en cada llamada (ok): %q vs %q", got, again)
	}
}
