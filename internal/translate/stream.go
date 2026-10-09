package translate

import (
	"bufio"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"strings"

	"github.com/ruxwez/claude-proxy/internal/anthropic"
	"github.com/ruxwez/claude-proxy/internal/openai"
)

// OpenAIStreamToAnthropic lee el SSE de OpenAI (resp.Body) y escribe SSE
// con eventos Anthropic en w. Cubre texto y tool_calls (partial_json).
func OpenAIStreamToAnthropic(body io.Reader, w http.ResponseWriter, origModel string) error {
	flusher, ok := w.(http.Flusher)
	if !ok {
		return fmt.Errorf("streaming no soportado por el writer")
	}

	msgID := NewMessageID()
	writeEvent := func(event string, payload any) {
		b, _ := json.Marshal(payload)
		fmt.Fprintf(w, "event: %s\ndata: %s\n\n", event, string(b))
		flusher.Flush()
	}

	// message_start con usage de entrada desconocido (se actualiza al final).
	writeEvent("message_start", map[string]any{
		"type": "message_start",
		"message": map[string]any{
			"id":            msgID,
			"type":          "message",
			"role":          "assistant",
			"content":       []any{},
			"model":         origModel,
			"stop_reason":   nil,
			"stop_sequence": nil,
			"usage":         map[string]any{"input_tokens": 0, "output_tokens": 0},
		},
	})

	textOpen := false
	textIndex := 0
	openText := func() {
		if textOpen {
			return
		}
		writeEvent("content_block_start", map[string]any{
			"type":  "content_block_start",
			"index": textIndex,
			"content_block": map[string]any{
				"type": "text",
				"text": "",
			},
		})
		textOpen = true
	}
	closeText := func() {
		if !textOpen {
			return
		}
		writeEvent("content_block_stop", map[string]any{
			"type":  "content_block_stop",
			"index": textIndex,
		})
		textOpen = false
		textIndex++
	}

	type toolState struct {
		index int
		id    string
		name  string
		open  bool
	}
	tools := map[int]*toolState{} // key: openai tool index
	nextIndex := 1                // 0 reservado para texto (si hay)
	ensureTool := func(oi int) *toolState {
		if st, ok := tools[oi]; ok {
			return st
		}
		st := &toolState{index: nextIndex}
		nextIndex++
		tools[oi] = st
		return st
	}

	inputTokens, outputTokens := 0, 0
	stopReason := "end_turn"

	sc := bufio.NewScanner(body)
	sc.Buffer(make([]byte, 1024*1024), 1024*1024)
	for sc.Scan() {
		line := strings.TrimSpace(sc.Text())
		if line == "" || strings.HasPrefix(line, ":") {
			continue
		}
		if !strings.HasPrefix(line, "data:") {
			continue
		}
		data := strings.TrimSpace(strings.TrimPrefix(line, "data:"))
		if data == "[DONE]" {
			break
		}
		var chunk openai.Chunk
		if err := json.Unmarshal([]byte(data), &chunk); err != nil {
			continue // línea no-JSON: ignorar
		}
		if chunk.Usage != nil {
			inputTokens = chunk.Usage.PromptTokens
			outputTokens = chunk.Usage.CompletionTokens
		}
		for _, ch := range chunk.Choices {
			if ch.FinishReason != nil {
				switch *ch.FinishReason {
				case "tool_calls":
					stopReason = "tool_use"
				case "length":
					stopReason = "max_tokens"
				default:
					stopReason = "end_turn"
				}
			}
			d := ch.Delta
			if d.Content != nil && *d.Content != "" {
				openText()
				writeEvent("content_block_delta", map[string]any{
					"type":  "content_block_delta",
					"index": textIndex,
					"delta": map[string]any{
						"type": "text_delta",
						"text": *d.Content,
					},
				})
			}
			for _, tc := range d.ToolCalls {
				oi := 0
				if tc.Index != nil {
					oi = *tc.Index
				}
				st := ensureTool(oi)
				if tc.ID != nil && *tc.ID != "" {
					st.id = *tc.ID
				}
				if tc.Function != nil && tc.Function.Name != nil && *tc.Function.Name != "" {
					st.name = *tc.Function.Name
				}
				if !st.open && st.name != "" {
					closeText()
					writeEvent("content_block_start", map[string]any{
						"type":  "content_block_start",
						"index": st.index,
						"content_block": map[string]any{
							"type":  "tool_use",
							"id":    st.id,
							"name":  st.name,
							"input": map[string]any{},
						},
					})
					st.open = true
				}
				if tc.Function != nil && tc.Function.Arguments != nil && *tc.Function.Arguments != "" && st.open {
					writeEvent("content_block_delta", map[string]any{
						"type":  "content_block_delta",
						"index": st.index,
						"delta": map[string]any{
							"type":         "input_json_delta",
							"partial_json": *tc.Function.Arguments,
						},
					})
				}
			}
		}
	}
	// Cerrar bloques abiertos en orden de índice.
	closeText()
	ordered := make([]*toolState, 0, len(tools))
	for _, st := range tools {
		if st.open {
			ordered = append(ordered, st)
		}
	}
	// Orden simple por índice (burbuja, n pequeño).
	for i := 0; i < len(ordered); i++ {
		for j := i + 1; j < len(ordered); j++ {
			if ordered[j].index < ordered[i].index {
				ordered[i], ordered[j] = ordered[j], ordered[i]
			}
		}
	}
	for _, st := range ordered {
		writeEvent("content_block_stop", map[string]any{
			"type":  "content_block_stop",
			"index": st.index,
		})
	}

	writeEvent("message_delta", map[string]any{
		"type": "message_delta",
		"delta": map[string]any{
			"stop_reason":   stopReason,
			"stop_sequence": nil,
		},
		"usage": map[string]any{"output_tokens": outputTokens},
	})
	// message_stop + ping final para compatibilidad con algunos SDKs.
	writeEvent("message_stop", map[string]any{"type": "message_stop"})
	_ = anthropic.Usage{InputTokens: inputTokens, OutputTokens: outputTokens}
	return sc.Err()
}
