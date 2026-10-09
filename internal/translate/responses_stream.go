package translate

import (
	"bufio"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"strings"

	"github.com/ruxwez/claude-proxy/internal/responses"
)

// ResponsesStreamToAnthropic lee el SSE de Responses API (resp.Body) y escribe
// SSE con eventos Anthropic en w. Cubre texto y function_call (partial_json).
func ResponsesStreamToAnthropic(body io.Reader, w http.ResponseWriter, origModel string) error {
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
	toolsByIndex := map[int]*toolState{}
	toolsByItem := map[string]*toolState{}
	nextIndex := 1 // 0 reservado para texto
	ensureTool := func(outputIndex int, itemID string) *toolState {
		if itemID != "" {
			if st, ok := toolsByItem[itemID]; ok {
				return st
			}
		}
		if st, ok := toolsByIndex[outputIndex]; ok {
			if itemID != "" {
				toolsByItem[itemID] = st
			}
			return st
		}
		st := &toolState{index: nextIndex}
		nextIndex++
		toolsByIndex[outputIndex] = st
		if itemID != "" {
			toolsByItem[itemID] = st
		}
		return st
	}
	lookupTool := func(outputIndex int, itemID string) *toolState {
		if itemID != "" {
			if st, ok := toolsByItem[itemID]; ok {
				return st
			}
		}
		if st, ok := toolsByIndex[outputIndex]; ok {
			return st
		}
		return nil
	}
	outputIndexOf := func(ev responses.StreamEvent) int {
		if ev.OutputIndex != nil {
			return *ev.OutputIndex
		}
		return 0
	}
	openTool := func(st *toolState) {
		if st.open || st.name == "" {
			return
		}
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

	inputTokens, outputTokens := 0, 0
	stopReason := "end_turn"
	sawToolCall := false

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
		var ev responses.StreamEvent
		if err := json.Unmarshal([]byte(data), &ev); err != nil {
			continue
		}
		switch ev.Type {
		case "response.output_text.delta":
			if ev.Delta != "" {
				openText()
				writeEvent("content_block_delta", map[string]any{
					"type":  "content_block_delta",
					"index": textIndex,
					"delta": map[string]any{
						"type": "text_delta",
						"text": ev.Delta,
					},
				})
			}
		case "response.function_call_arguments.delta":
			if ev.Delta == "" {
				continue
			}
			st := lookupTool(outputIndexOf(ev), ev.ItemID)
			if st == nil {
				st = ensureTool(outputIndexOf(ev), ev.ItemID)
			}
			openTool(st)
			if st.open {
				writeEvent("content_block_delta", map[string]any{
					"type":  "content_block_delta",
					"index": st.index,
					"delta": map[string]any{
						"type":         "input_json_delta",
						"partial_json": ev.Delta,
					},
				})
			}
		case "response.output_item.added":
			if ev.Item == nil || ev.Item.Type != "function_call" {
				continue
			}
			sawToolCall = true
			st := ensureTool(outputIndexOf(ev), ev.Item.ID)
			if ev.Item.Name != "" {
				st.name = ev.Item.Name
			}
			if ev.Item.CallID != "" {
				st.id = ev.Item.CallID
			} else if ev.Item.ID != "" && st.id == "" {
				st.id = ev.Item.ID
			}
			openTool(st)
		case "response.output_item.done":
			if ev.Item == nil || ev.Item.Type != "function_call" {
				continue
			}
			sawToolCall = true
			st := ensureTool(outputIndexOf(ev), ev.Item.ID)
			if ev.Item.Name != "" {
				st.name = ev.Item.Name
			}
			if ev.Item.CallID != "" {
				st.id = ev.Item.CallID
			} else if ev.Item.ID != "" && st.id == "" {
				st.id = ev.Item.ID
			}
			// Algunos servidores envían los arguments completos solo en .done.
			if !st.open {
				openTool(st)
			}
			if st.open && ev.Item.Arguments != "" {
				writeEvent("content_block_delta", map[string]any{
					"type":  "content_block_delta",
					"index": st.index,
					"delta": map[string]any{
						"type":         "input_json_delta",
						"partial_json": ev.Item.Arguments,
					},
				})
			}
		case "response.completed", "response.incomplete":
			if ev.Response != nil {
				if ev.Response.Usage != nil {
					inputTokens = ev.Response.Usage.InputTokens
					outputTokens = ev.Response.Usage.OutputTokens
				}
				if ev.Type == "response.incomplete" &&
					ev.Response.IncompleteDetails != nil &&
					ev.Response.IncompleteDetails.Reason == "max_output_tokens" {
					stopReason = "max_tokens"
				}
			}
		case "response.failed":
			stopReason = "end_turn"
		}
	}
	if sawToolCall {
		stopReason = "tool_use"
	}

	closeText()
	// Cerrar herramientas abiertas en orden de índice.
	maxIdx := nextIndex
	for i := 1; i < maxIdx; i++ {
		for _, st := range toolsByIndex {
			if st.open && st.index == i {
				writeEvent("content_block_stop", map[string]any{
					"type":  "content_block_stop",
					"index": st.index,
				})
				st.open = false
			}
		}
	}

	writeEvent("message_delta", map[string]any{
		"type": "message_delta",
		"delta": map[string]any{
			"stop_reason":   stopReason,
			"stop_sequence": nil,
		},
		"usage": map[string]any{"output_tokens": outputTokens},
	})
	writeEvent("message_stop", map[string]any{"type": "message_stop"})
	_ = inputTokens
	return sc.Err()
}
