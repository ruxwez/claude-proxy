package translate

import (
	"encoding/json"
	"fmt"
	"strings"

	"github.com/ruxwez/claude-proxy/internal/anthropic"
	"github.com/ruxwez/claude-proxy/internal/responses"
)

// AnthropicToResponses convierte un MessagesRequest a Responses API request.
// No soportado: stop_sequences y top_k (Responses API no los contempla;
// se ignoran en lugar de fallar).
func AnthropicToResponses(req *anthropic.MessagesRequest, targetModel string) (*responses.Request, error) {
	if req == nil {
		return nil, fmt.Errorf("petición nil")
	}
	store := false
	out := &responses.Request{
		Model:  targetModel,
		Stream: req.Stream,
		Store:  &store,
		Input:  []any{},
	}

	if sys := req.System.Text(); sys != "" {
		out.Input = append(out.Input, map[string]any{
			"role": "system",
			"content": []any{
				map[string]any{"type": "input_text", "text": sys},
			},
		})
	}

	for i, m := range req.Messages {
		items, err := convertMessageToResponsesInput(m)
		if err != nil {
			return nil, fmt.Errorf("messages[%d]: %w", i, err)
		}
		out.Input = append(out.Input, items...)
	}

	if req.MaxTokens > 0 {
		m := req.MaxTokens
		out.MaxOutputTokens = &m
	}
	if req.Temperature != nil {
		out.Temperature = req.Temperature
	}
	if req.TopP != nil {
		out.TopP = req.TopP
	}
	if len(req.Tools) > 0 {
		tools := make([]responses.Tool, 0, len(req.Tools))
		for _, t := range req.Tools {
			params := t.InputSchema
			if params == nil {
				params = map[string]any{"type": "object"}
			}
			tools = append(tools, responses.Tool{
				Type:        "function",
				Name:        t.Name,
				Description: t.Description,
				Parameters:  params,
			})
		}
		out.Tools = tools
	}
	if req.ToolChoice != nil {
		out.ToolChoice = convertToolChoice(req.ToolChoice)
	}
	return out, nil
}

// convertMessageToResponsesInput convierte un turno Anthropic en 1..N
// items de input de Responses API.
func convertMessageToResponsesInput(m anthropic.Message) ([]any, error) {
	switch m.Role {
	case "system":
		// Mensajes de sistema a mitad de conversación (Claude Code los usa).
		var text strings.Builder
		for _, b := range m.Content {
			if b.Type == "text" {
				text.WriteString(b.Text)
			}
		}
		return []any{map[string]any{
			"role": "system",
			"content": []any{
				map[string]any{"type": "input_text", "text": text.String()},
			},
		}}, nil
	case "user":
		var parts []any
		var out []any
		flush := func() {
			if len(parts) == 0 {
				return
			}
			out = append(out, map[string]any{"role": "user", "content": parts})
			parts = nil
		}
		for _, b := range m.Content {
			switch b.Type {
			case "text":
				parts = append(parts, map[string]any{"type": "input_text", "text": b.Text})
			case "image":
				url, err := responsesImageURL(b)
				if err != nil {
					return nil, err
				}
				parts = append(parts, map[string]any{"type": "input_image", "image_url": url})
			case "tool_result":
				flush()
				output := b.Content.Text()
				out = append(out, map[string]any{
					"type":    "function_call_output",
					"call_id": b.ToolUseID,
					"output":  output,
				})
			default:
				parts = append(parts, map[string]any{"type": "input_text", "text": b.Text})
			}
		}
		flush()
		if len(out) == 0 {
			out = append(out, map[string]any{
				"role":    "user",
				"content": []any{map[string]any{"type": "input_text", "text": ""}},
			})
		}
		return out, nil
	case "assistant":
		var text strings.Builder
		var out []any
		for _, b := range m.Content {
			switch b.Type {
			case "text":
				text.WriteString(b.Text)
			case "tool_use":
				args := string(b.Input)
				if args == "" {
					args = "{}"
				}
				out = append(out, map[string]any{
					"type":      "function_call",
					"call_id":   b.ID,
					"name":      b.Name,
					"arguments": args,
				})
			case "image":
				text.WriteString("[imagen]")
			default:
				text.WriteString(b.Text)
			}
		}
		if s := text.String(); s != "" {
			out = append([]any{map[string]any{
				"role":    "assistant",
				"content": []any{map[string]any{"type": "output_text", "text": s}},
			}}, out...)
		}
		if len(out) == 0 {
			out = append(out, map[string]any{
				"role":    "assistant",
				"content": []any{map[string]any{"type": "output_text", "text": ""}},
			})
		}
		return out, nil
	default:
		return nil, fmt.Errorf("role %q no soportado (user|assistant)", m.Role)
	}
}

func responsesImageURL(b anthropic.Block) (string, error) {
	if b.Source == nil {
		return "", fmt.Errorf("bloque image sin source")
	}
	switch b.Source.Type {
	case "base64":
		if b.Source.Data == "" || b.Source.MediaType == "" {
			return "", fmt.Errorf("image base64 requiere media_type y data")
		}
		return "data:" + b.Source.MediaType + ";base64," + b.Source.Data, nil
	case "url":
		if b.Source.URL == "" {
			return "", fmt.Errorf("image url vacía")
		}
		return b.Source.URL, nil
	default:
		return "", fmt.Errorf("image source type %q no soportado", b.Source.Type)
	}
}

// ResponsesToAnthropic convierte una respuesta no-stream de Responses API
// al formato Anthropic.
func ResponsesToAnthropic(resp *responses.Response, origModel string) (*anthropic.MessagesResponse, error) {
	if resp == nil {
		return nil, fmt.Errorf("respuesta Responses vacía")
	}
	content := make([]any, 0, 2)
	toolUse := false
	for _, item := range resp.Output {
		switch item.Type {
		case "message":
			for _, p := range item.Content {
				if p.Type == "output_text" && p.Text != "" {
					content = append(content, map[string]any{"type": "text", "text": p.Text})
				}
			}
		case "function_call":
			toolUse = true
			var input any = map[string]any{}
			if item.Arguments != "" {
				var v any
				if err := json.Unmarshal([]byte(item.Arguments), &v); err == nil {
					input = v
				}
			}
			id := item.CallID
			if id == "" {
				id = item.ID
			}
			content = append(content, map[string]any{
				"type":  "tool_use",
				"id":    id,
				"name":  item.Name,
				"input": input,
			})
		default:
			// reasoning y otros items internos: se omiten.
		}
	}
	if len(content) == 0 {
		content = append(content, map[string]any{"type": "text", "text": ""})
	}

	stop := "end_turn"
	if toolUse {
		stop = "tool_use"
	} else if resp.Status == "incomplete" && resp.IncompleteDetails != nil &&
		resp.IncompleteDetails.Reason == "max_output_tokens" {
		stop = "max_tokens"
	}
	usage := anthropic.Usage{}
	if resp.Usage != nil {
		usage.InputTokens = resp.Usage.InputTokens
		usage.OutputTokens = resp.Usage.OutputTokens
	}
	return &anthropic.MessagesResponse{
		ID:         NewMessageID(),
		Type:       "message",
		Role:       "assistant",
		Content:    content,
		Model:      origModel,
		StopReason: &stop,
		Usage:      usage,
	}, nil
}
