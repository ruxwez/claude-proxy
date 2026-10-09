package translate

import (
	"encoding/json"
	"strings"
	"testing"

	"github.com/ruxwez/claude-proxy/internal/anthropic"
	"github.com/ruxwez/claude-proxy/internal/responses"
)

func TestAnthropicToResponses_Basic(t *testing.T) {
	var req anthropic.MessagesRequest
	raw := `{
		"model": "exo-free",
		"max_tokens": 128,
		"system": "Sé breve.",
		"temperature": 0.5,
		"tools": [{"name": "get_weather", "description": "clima", "input_schema": {"type": "object"}}],
		"tool_choice": {"type": "auto"},
		"messages": [
			{"role": "user", "content": "Hola"},
			{"role": "assistant", "content": [
				{"type": "text", "text": "Consulto."},
				{"type": "tool_use", "id": "call_7", "name": "get_weather", "input": {"city": "Madrid"}}
			]},
			{"role": "user", "content": [
				{"type": "tool_result", "tool_use_id": "call_7", "content": "21C"}
			]}
		]
	}`
	if err := json.Unmarshal([]byte(raw), &req); err != nil {
		t.Fatal(err)
	}
	out, err := AnthropicToResponses(&req, "exo-free")
	if err != nil {
		t.Fatal(err)
	}
	if out.Model != "exo-free" {
		t.Fatalf("model = %q", out.Model)
	}
	if out.MaxOutputTokens == nil || *out.MaxOutputTokens != 128 {
		t.Fatalf("max_output_tokens no propagado: %+v", out.MaxOutputTokens)
	}
	if out.Store == nil || *out.Store {
		t.Fatalf("store debe ser false explícito")
	}
	if len(out.Tools) != 1 || out.Tools[0].Name != "get_weather" {
		t.Fatalf("tools: %+v", out.Tools)
	}
	ser, _ := json.Marshal(out)
	s := string(ser)
	for _, want := range []string{
		`"function_call"`, `"call_7"`, `"function_call_output"`, `"21C"`, `"input_text"`, `"Sé breve."`,
	} {
		if !strings.Contains(s, want) {
			t.Fatalf("petición sin %q:\n%s", want, s)
		}
	}
}

func TestResponsesToAnthropic_TextAndTools(t *testing.T) {
	resp := &responses.Response{
		ID:     "resp_1",
		Model:  "muse-spark-1.3-contributor-free",
		Status: "completed",
		Output: []responses.OutputItem{
			{
				Type:   "message",
				Role:   "assistant",
				Status: "completed",
				Content: []responses.ContentPart{
					{Type: "output_text", Text: "Hola"},
				},
			},
			{
				Type:      "function_call",
				CallID:    "call_1",
				Name:      "get_weather",
				Arguments: `{"city":"Madrid"}`,
				Status:    "completed",
			},
		},
		Usage: &responses.Usage{InputTokens: 9, OutputTokens: 6, TotalTokens: 15},
	}
	out, err := ResponsesToAnthropic(resp, "muse-spark-1.3-contributor-free")
	if err != nil {
		t.Fatal(err)
	}
	if out.Model != "muse-spark-1.3-contributor-free" {
		t.Fatalf("model debe ser el original, got %q", out.Model)
	}
	if len(out.Content) != 2 {
		t.Fatalf("content = %d, want 2", len(out.Content))
	}
	if out.StopReason == nil || *out.StopReason != "tool_use" {
		t.Fatalf("stop_reason = %v, want tool_use", out.StopReason)
	}
	if out.Usage.InputTokens != 9 || out.Usage.OutputTokens != 6 {
		t.Fatalf("usage = %+v", out.Usage)
	}
}

func TestResponsesToAnthropic_MaxTokens(t *testing.T) {
	resp := &responses.Response{
		Status:            "incomplete",
		IncompleteDetails: &responses.IncompleteDetails{Reason: "max_output_tokens"},
		Output: []responses.OutputItem{{
			Type: "message", Role: "assistant",
			Content: []responses.ContentPart{{Type: "output_text", Text: "cortado"}},
		}},
	}
	out, err := ResponsesToAnthropic(resp, "m")
	if err != nil {
		t.Fatal(err)
	}
	if *out.StopReason != "max_tokens" {
		t.Fatalf("stop_reason = %q", *out.StopReason)
	}
}
