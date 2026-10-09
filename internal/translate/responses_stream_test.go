package translate

import (
	"net/http/httptest"
	"strings"
	"testing"
)

func TestResponsesStreamToAnthropic_Text(t *testing.T) {
	sse := `data: {"type":"response.output_text.delta","output_index":0,"content_index":0,"delta":"Hola"}
data: {"type":"response.output_text.delta","output_index":0,"content_index":0,"delta":" mundo"}
data: {"type":"response.completed","response":{"id":"resp_1","status":"completed","usage":{"input_tokens":5,"output_tokens":3,"total_tokens":8}}}
data: [DONE]
`
	rec := httptest.NewRecorder()
	if err := ResponsesStreamToAnthropic(strings.NewReader(sse), rec, "exo-free"); err != nil {
		t.Fatal(err)
	}
	body := rec.Body.String()
	for _, want := range []string{
		"event: message_start",
		"event: content_block_start",
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

func TestResponsesStreamToAnthropic_ToolCall(t *testing.T) {
	sse := `data: {"type":"response.output_item.added","output_index":1,"item":{"type":"function_call","id":"fc_1","call_id":"call_9","name":"get_weather","status":"in_progress"}}
data: {"type":"response.function_call_arguments.delta","output_index":1,"item_id":"fc_1","delta":"{\"city\":"}
data: {"type":"response.function_call_arguments.delta","output_index":1,"item_id":"fc_1","delta":"\"Madrid\"}"}
data: {"type":"response.completed","response":{"id":"resp_2","status":"completed","usage":{"input_tokens":8,"output_tokens":6,"total_tokens":14}}}
data: [DONE]
`
	rec := httptest.NewRecorder()
	if err := ResponsesStreamToAnthropic(strings.NewReader(sse), rec, "muse-spark-1.3-contributor-free"); err != nil {
		t.Fatal(err)
	}
	body := rec.Body.String()
	for _, want := range []string{
		`"type":"tool_use"`,
		`"name":"get_weather"`,
		`"id":"call_9"`,
		"input_json_delta",
		`"stop_reason":"tool_use"`,
	} {
		if !strings.Contains(body, want) {
			t.Fatalf("SSE tool sin %q:\n%s", want, body)
		}
	}
}

func TestResponsesStreamToAnthropic_DoneOnlyArgs(t *testing.T) {
	// Servidores que mandan arguments completos solo en output_item.done.
	sse := `data: {"type":"response.output_item.done","output_index":0,"item":{"type":"function_call","id":"fc_2","call_id":"call_2","name":"f","arguments":"{\"a\":1}","status":"completed"}}
data: {"type":"response.completed","response":{"id":"r","status":"completed"}}
data: [DONE]
`
	rec := httptest.NewRecorder()
	if err := ResponsesStreamToAnthropic(strings.NewReader(sse), rec, "m"); err != nil {
		t.Fatal(err)
	}
	body := rec.Body.String()
	if !strings.Contains(body, `"stop_reason":"tool_use"`) {
		t.Fatalf("esperaba tool_use:\n%s", body)
	}
}
