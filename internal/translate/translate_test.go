package translate

import (
	"encoding/json"
	"strings"
	"testing"

	"github.com/ruxwez/claude-proxy/internal/anthropic"
	"github.com/ruxwez/claude-proxy/internal/openai"
)

func f64(v float64) *float64 { return &v }

func TestAnthropicToOpenAI_Basic(t *testing.T) {
	var req anthropic.MessagesRequest
	raw := `{
		"model": "claude-3-5-sonnet-20241022",
		"max_tokens": 256,
		"system": "Responde en español.",
		"temperature": 0.7,
		"stop_sequences": ["STOP"],
		"messages": [{"role": "user", "content": "Hola"}]
	}`
	if err := json.Unmarshal([]byte(raw), &req); err != nil {
		t.Fatal(err)
	}
	out, err := AnthropicToOpenAI(&req, "gpt-4o-mini")
	if err != nil {
		t.Fatal(err)
	}
	if out.Model != "gpt-4o-mini" {
		t.Fatalf("model = %q", out.Model)
	}
	if len(out.Messages) != 2 {
		t.Fatalf("messages = %d, want 2 (system+user)", len(out.Messages))
	}
	if out.Messages[0].Role != "system" {
		t.Fatalf("messages[0].role = %q", out.Messages[0].Role)
	}
	if out.MaxTokens == nil || *out.MaxTokens != 256 {
		t.Fatalf("max_tokens no propagado")
	}
	if out.Temperature == nil || *out.Temperature != 0.7 {
		t.Fatalf("temperature no propagada")
	}
	if len(out.Stop) != 1 || out.Stop[0] != "STOP" {
		t.Fatalf("stop no propagado: %v", out.Stop)
	}
}

func TestAnthropicToOpenAI_ToolsAndToolResult(t *testing.T) {
	raw := `{
		"model": "claude-x",
		"max_tokens": 100,
		"tools": [{"name": "get_weather", "description": "clima", "input_schema": {"type": "object"}}],
		"tool_choice": {"type": "tool", "name": "get_weather"},
		"messages": [
			{"role": "user", "content": "clima en Madrid?"},
			{"role": "assistant", "content": [
				{"type": "text", "text": "Voy a consultar."},
				{"type": "tool_use", "id": "toolu_1", "name": "get_weather", "input": {"city": "Madrid"}}
			]},
			{"role": "user", "content": [
				{"type": "tool_result", "tool_use_id": "toolu_1", "content": "21C soleado"}
			]}
		]
	}`
	var req anthropic.MessagesRequest
	if err := json.Unmarshal([]byte(raw), &req); err != nil {
		t.Fatal(err)
	}
	out, err := AnthropicToOpenAI(&req, "gpt-4o")
	if err != nil {
		t.Fatal(err)
	}
	if len(out.Tools) != 1 || out.Tools[0].Function.Name != "get_weather" {
		t.Fatalf("tools no convertidas: %+v", out.Tools)
	}
	// tool_choice tool -> function
	m, ok := out.ToolChoice.(map[string]any)
	if !ok {
		t.Fatalf("tool_choice tipo %T, want map", out.ToolChoice)
	}
	if m["type"] != "function" {
		t.Fatalf("tool_choice.type = %v", m["type"])
	}
	// assistant con tool_calls
	found := false
	for _, msg := range out.Messages {
		if msg.Role == "assistant" && len(msg.ToolCalls) == 1 {
			found = true
			if msg.ToolCalls[0].ID != "toolu_1" {
				t.Fatalf("tool_call id = %q", msg.ToolCalls[0].ID)
			}
		}
		if msg.Role == "tool" && msg.ToolCallID == "toolu_1" {
			found = found && true
			if s, _ := msg.Content.(string); !strings.Contains(s, "21C") {
				t.Fatalf("tool content = %v", msg.Content)
			}
		}
	}
	if !found {
		t.Fatalf("no se encontraron tool_calls/tool: %+v", out.Messages)
	}
}

func TestOpenAIToAnthropic_TextAndTools(t *testing.T) {
	fr := "tool_calls"
	resp := &openai.ChatResponse{
		Model: "gpt-4o-mini",
		Choices: []openai.Choice{{
			Message: openai.ChatMessage{
				Role:    "assistant",
				Content: "Hola",
				ToolCalls: []openai.ToolCall{{
					ID:   "call_1",
					Type: "function",
					Function: openai.ToolCallFunction{
						Name:      "get_weather",
						Arguments: `{"city":"Madrid"}`,
					},
				}},
			},
			FinishReason: &fr,
		}},
		Usage: &openai.Usage{PromptTokens: 10, CompletionTokens: 5, TotalTokens: 15},
	}
	out, err := OpenAIToAnthropic(resp, "claude-3-5-sonnet-20241022")
	if err != nil {
		t.Fatal(err)
	}
	if out.Model != "claude-3-5-sonnet-20241022" {
		t.Fatalf("model debe ser el original anthropic, got %q", out.Model)
	}
	if len(out.Content) != 2 {
		t.Fatalf("content = %d, want 2 (text+tool_use)", len(out.Content))
	}
	if out.Usage.InputTokens != 10 || out.Usage.OutputTokens != 5 {
		t.Fatalf("usage = %+v", out.Usage)
	}
	if out.StopReason == nil || *out.StopReason != "tool_use" {
		t.Fatalf("stop_reason = %v, want tool_use", out.StopReason)
	}
	second := out.Content[1].(map[string]any)
	if second["type"] != "tool_use" || second["name"] != "get_weather" {
		t.Fatalf("tool_use malformado: %v", second)
	}
}

func TestSystemArray(t *testing.T) {
	var req anthropic.MessagesRequest
	raw := `{
		"model": "m", "max_tokens": 10,
		"system": [{"type": "text", "text": "a"}, {"type": "text", "text": "b"}],
		"messages": [{"role": "user", "content": "hi"}]
	}`
	if err := json.Unmarshal([]byte(raw), &req); err != nil {
		t.Fatal(err)
	}
	if req.System.Text() != "a\nb" {
		t.Fatalf("system = %q", req.System.Text())
	}
	_ = f64
}
