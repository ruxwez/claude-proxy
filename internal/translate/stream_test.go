package translate

import (
	"net/http/httptest"
	"strings"
	"testing"
)

func TestOpenAIStreamToAnthropic_Text(t *testing.T) {
	sse := `data: {"id":"chatcmpl-1","object":"chat.completion.chunk","created":1,"model":"gpt-4o-mini","choices":[{"index":0,"delta":{"role":"assistant","content":"Hola"},"finish_reason":null}]}
data: {"id":"chatcmpl-1","object":"chat.completion.chunk","created":1,"model":"gpt-4o-mini","choices":[{"index":0,"delta":{"content":" mundo"},"finish_reason":null}]}
data: {"id":"chatcmpl-1","object":"chat.completion.chunk","created":1,"model":"gpt-4o-mini","choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":5,"completion_tokens":3,"total_tokens":8}}
data: [DONE]
`
	rec := httptest.NewRecorder()
	// Simular Flusher de httptest (ResponseRecorder lo implementa).
	if err := OpenAIStreamToAnthropic(strings.NewReader(sse), rec, "claude-3-5-sonnet-20241022"); err != nil {
		t.Fatal(err)
	}
	body := rec.Body.String()
	for _, want := range []string{
		"event: message_start",
		"event: content_block_start",
		"event: content_block_delta",
		`"text":"Hola"`,
		"event: content_block_stop",
		"event: message_delta",
		`"stop_reason":"end_turn"`,
		"event: message_stop",
	} {
		if !strings.Contains(body, want) {
			t.Fatalf("SSE sin %q:\n%s", want, body)
		}
	}
}

func TestOpenAIStreamToAnthropic_ToolCall(t *testing.T) {
	sse := `data: {"id":"c1","object":"chat.completion.chunk","created":1,"model":"gpt-4o","choices":[{"index":0,"delta":{"role":"assistant","tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"get_weather","arguments":""}}]},"finish_reason":null}]}
data: {"id":"c1","object":"chat.completion.chunk","created":1,"model":"gpt-4o","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"city\":"}}]},"finish_reason":null}]}
data: {"id":"c1","object":"chat.completion.chunk","created":1,"model":"gpt-4o","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"Madrid\"}"}}]},"finish_reason":"tool_calls"}]}
data: [DONE]
`
	rec := httptest.NewRecorder()
	if err := OpenAIStreamToAnthropic(strings.NewReader(sse), rec, "claude-x"); err != nil {
		t.Fatal(err)
	}
	body := rec.Body.String()
	for _, want := range []string{
		`"type":"tool_use"`,
		`"name":"get_weather"`,
		"input_json_delta",
		`"stop_reason":"tool_use"`,
	} {
		if !strings.Contains(body, want) {
			t.Fatalf("SSE tool sin %q:\n%s", want, body)
		}
	}
}
