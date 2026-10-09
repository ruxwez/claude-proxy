package opencode

import (
	"context"
	"io"
	"net/http"
	"net/http/httptest"
	"regexp"
	"strings"
	"testing"
	"time"

	"github.com/ruxwez/claude-proxy/internal/anthropic"
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

func TestClientUserAgent(t *testing.T) {
	// El User-Agent debe empezar por opencode/<semver>: la API rechaza con
	// 403 cualquier otro formato (o su ausencia).
	uaRe := regexp.MustCompile(`^opencode/\d+\.\d+\.\d+ `)

	if got := clientUserAgent(""); !uaRe.MatchString(got) {
		t.Fatalf("clientUserAgent(\"\") = %q, no empieza por opencode/<versión>", got)
	}
	if got := clientUserAgent("  1.20.0  "); !strings.HasPrefix(got, "opencode/1.20.0 ") {
		t.Fatalf("clientUserAgent con versión = %q", got)
	}
	if got := clientUserAgent(""); got != clientUserAgent("2.0.26") {
		t.Fatalf("versión vacía debería usar la de por defecto: %q", got)
	}
}

func TestIsIdentifier(t *testing.T) {
	// Formato real: prefijo + 12 hex + 14 base62.
	if !isSessionID("ses_ee18f6c6ffffABCDEFGHIJKLMN") {
		t.Fatal("ID de sesión OpenCode válido rechazado")
	}
	if !isIdentifier("msg_121e4b880003ABCDEFGHIJKLMN", idPrefixMessage) {
		t.Fatal("ID de mensaje OpenCode válido rechazado")
	}
	for _, bad := range []string{
		"",
		"ses_",
		"ses_ee18f6c6ffff",                   // sin parte aleatoria
		"ses_ee18f6c6ffffABCDEFGHIJKLMNOPQR", // aleatoria demasiado larga
		"ses_ZZ18f6c6ffffABCDEFGHIJKLMN",     // Z no es hex
		"ses_ee18f6c6ffffABCDEFGHIJKLM_",     // _ no es base62
		"01af6a19-06a4-7c8c-92b0-0a1e7f1f5a45", // UUID de Claude Code
		"ee18f6c6ffffABCDEFGHIJKLMN",          // sin prefijo
		"SES_ee18f6c6ffffABCDEFGHIJKLMN",      // prefijo en mayúsculas
	} {
		if isSessionID(bad) {
			t.Errorf("isSessionID(%q) = true, want false", bad)
		}
	}
	if isIdentifier("ses_ee18f6c6ffffABCDEFGHIJKLMN", idPrefixMessage) {
		t.Error("un ID de sesión no debería validar como mensaje")
	}
}

// TestIDTimeHexParity fija la salida contra el algoritmo de referencia de
// OpenCode (packages/opencode/src/id/id.ts): ms*4096 + counter en 6 bytes
// big-endian, con los bits invertidos para los IDs descendentes.
//
//	ms=1791570000000, counter=3
//	  ascendente (msg_):  121e4b880003
//	  descendente (ses_): ede1b477fffc
func TestIDTimeHexParity(t *testing.T) {
	const ms, counter = 1791570000000, 3
	if got := idTimeHex(ms, counter, false); got != "121e4b880003" {
		t.Errorf("idTimeHex ascendente = %q, want 121e4b880003", got)
	}
	if got := idTimeHex(ms, counter, true); got != "ede1b477fffc" {
		t.Errorf("idTimeHex descendente = %q, want ede1b477fffc", got)
	}
}

func TestSessionIDFromHeaders(t *testing.T) {
	// Un ID ya en formato OpenCode se reenvía tal cual.
	r := httptest.NewRequest("POST", "/v1/messages", nil)
	r.Header.Set("x-opencode-session", "ses_ee18f6c6ffffABCDEFGHIJKLMN")
	if got := sessionID(r); got != "ses_ee18f6c6ffffABCDEFGHIJKLMN" {
		t.Fatalf("sessionID = %q, debería reenviar el ID nativo", got)
	}

	// Un ID ajeno (el UUID de Claude Code) se convierte al formato de
	// OpenCode, de forma estable entre peticiones.
	r2 := httptest.NewRequest("POST", "/v1/messages", nil)
	r2.Header.Set("X-Claude-Code-Session-Id", "01af6a19-06a4-7c8c-92b0-0a1e7f1f5a45")
	got := sessionID(r2)
	if !isSessionID(got) {
		t.Fatalf("sessionID = %q, no tiene formato OpenCode", got)
	}
	if again := sessionID(r2); again != got {
		t.Fatalf("derivación no estable: %q vs %q", got, again)
	}

	// Sin cabeceras: se genera un ID nuevo en formato OpenCode.
	r3 := httptest.NewRequest("POST", "/v1/messages", nil)
	got3 := sessionID(r3)
	if !isSessionID(got3) {
		t.Fatalf("sessionID generado = %q, no tiene formato OpenCode", got3)
	}
}

func TestNewMessageIDFormat(t *testing.T) {
	// x-opencode-request lleva el ID del mensaje de usuario: msg_ + 12 hex
	// + 14 base62 (ascendente), uno distinto por petición.
	a, b := newMessageID(), newMessageID()
	for _, id := range []string{a, b} {
		if !isIdentifier(id, idPrefixMessage) {
			t.Fatalf("newMessageID = %q, formato inválido", id)
		}
	}
	if a == b {
		t.Fatalf("dos IDs de mensaje idénticos: %q", a)
	}
}

func TestChatSendsClientFingerprint(t *testing.T) {
	var gotUA, gotProject, gotSession, gotRequest, gotClient, gotAuth string
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		gotUA = r.Header.Get("User-Agent")
		gotProject = r.Header.Get("x-opencode-project")
		gotSession = r.Header.Get("x-opencode-session")
		gotRequest = r.Header.Get("x-opencode-request")
		gotClient = r.Header.Get("x-opencode-client")
		gotAuth = r.Header.Get("Authorization")
		w.Header().Set("Content-Type", "application/json")
		io.WriteString(w, `{"id":"chatcmpl-1","object":"chat.completion","choices":[{"index":0,"message":{"role":"assistant","content":"hola"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}`)
	}))
	defer srv.Close()

	c := newConnector("opencode-zen", srv.URL, "test-key", "", "proj-fijo", false, 5*time.Second)
	req := &anthropic.MessagesRequest{
		Model:     "opencode-zen/glm-test",
		MaxTokens: 16,
		Messages:  []anthropic.Message{{Role: "user", Content: anthropic.MessageContent{{Type: "text", Text: "hola"}}}},
	}
	r := httptest.NewRequest("POST", "/v1/messages", nil)
	r.Header.Set("X-Claude-Code-Session-Id", "01af6a19-06a4-7c8c-92b0-0a1e7f1f5a45")
	rec := httptest.NewRecorder()

	if err := c.Chat(context.Background(), req, "glm-test", rec, r); err != nil {
		t.Fatalf("Chat: %v", err)
	}
	if rec.Code != http.StatusOK {
		t.Fatalf("status = %d, want 200: %s", rec.Code, rec.Body.String())
	}
	if !strings.HasPrefix(gotUA, "opencode/") {
		t.Errorf("User-Agent saliente = %q, want opencode/<versión>", gotUA)
	}
	if gotProject != "proj-fijo" {
		t.Errorf("x-opencode-project = %q, want proj-fijo", gotProject)
	}
	if !isSessionID(gotSession) {
		t.Errorf("x-opencode-session = %q, formato OpenCode inválido", gotSession)
	}
	if !isIdentifier(gotRequest, idPrefixMessage) {
		t.Errorf("x-opencode-request = %q, formato OpenCode inválido", gotRequest)
	}
	if gotClient != "cli" {
		t.Errorf("x-opencode-client = %q, want cli", gotClient)
	}
	if !strings.HasPrefix(gotAuth, "Bearer ") {
		t.Errorf("Authorization = %q, want Bearer key", gotAuth)
	}
}

func TestModelsSendsClientFingerprint(t *testing.T) {
	var gotUA, gotClient string
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		gotUA = r.Header.Get("User-Agent")
		gotClient = r.Header.Get("x-opencode-client")
		w.Header().Set("Content-Type", "application/json")
		io.WriteString(w, `{"data":[{"id":"glm-test","created":1}]}`)
	}))
	defer srv.Close()

	c := newConnector("opencode-zen", srv.URL, "test-key", "1.20.0", "", false, 5*time.Second)
	if _, err := c.Models(context.Background()); err != nil {
		t.Fatalf("Models: %v", err)
	}
	if !strings.HasPrefix(gotUA, "opencode/1.20.0 ") {
		t.Errorf("User-Agent saliente = %q, want opencode/1.20.0 ...", gotUA)
	}
	if gotClient != "cli" {
		t.Errorf("x-opencode-client = %q, want cli", gotClient)
	}
}

func TestProjectIDGenerated(t *testing.T) {
	if got := projectIDOrGenerated("  abc123  "); got != "abc123" {
		t.Errorf("projectIDOrGenerated con valor = %q, want abc123", got)
	}
	// Sin valor: hash estable dentro de la misma llamada y no vacío.
	a, b := projectIDOrGenerated(""), projectIDOrGenerated("")
	if a == "" || b == "" {
		t.Fatal("projectID generado vacío")
	}
	if len(a) != 40 {
		t.Errorf("projectID generado = %q, want 40 chars hex", a)
	}
}
