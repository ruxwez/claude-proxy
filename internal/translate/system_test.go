package translate

import (
	"testing"

	"github.com/ruxwez/claude-proxy/internal/anthropic"
)

// midConversationSystem devuelve una petición con un mensaje system entre
// turnos, como los que envía Claude Code.
func midConversationSystem() *anthropic.MessagesRequest {
	return &anthropic.MessagesRequest{
		Model:     "opencode-go/minimax-m3",
		MaxTokens: 16,
		Messages: []anthropic.Message{
			{Role: "user", Content: anthropic.MessageContent{{Type: "text", Text: "hola"}}},
			{Role: "system", Content: anthropic.MessageContent{{Type: "text", Text: "se breve"}}},
			{Role: "user", Content: anthropic.MessageContent{{Type: "text", Text: "adios"}}},
		},
	}
}

func TestAnthropicToOpenAIMidConversationSystem(t *testing.T) {
	out, err := AnthropicToOpenAI(midConversationSystem(), "target")
	if err != nil {
		t.Fatalf("error: %v", err)
	}
	if len(out.Messages) != 3 {
		t.Fatalf("mensajes = %d, want 3", len(out.Messages))
	}
	if out.Messages[1].Role != "system" {
		t.Fatalf("messages[1].role = %q, want system", out.Messages[1].Role)
	}
	if s, _ := out.Messages[1].Content.(string); s != "se breve" {
		t.Fatalf("messages[1].content = %v, want se breve", out.Messages[1].Content)
	}
}

func TestAnthropicToResponsesMidConversationSystem(t *testing.T) {
	out, err := AnthropicToResponses(midConversationSystem(), "target")
	if err != nil {
		t.Fatalf("error: %v", err)
	}
	if len(out.Input) != 3 {
		t.Fatalf("input = %d, want 3", len(out.Input))
	}
	item, ok := out.Input[1].(map[string]any)
	if !ok {
		t.Fatalf("input[1] no es map: %T", out.Input[1])
	}
	if item["role"] != "system" {
		t.Fatalf("input[1].role = %v, want system", item["role"])
	}
}
