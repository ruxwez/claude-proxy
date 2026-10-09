// Package translate convierte entre el estándar Anthropic y el de OpenAI.
package translate

import (
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"strings"

	"github.com/ruxwez/claude-proxy/internal/anthropic"
	"github.com/ruxwez/claude-proxy/internal/openai"
)

// NewMessageID genera un id tipo Anthropic: msg_<24 hex>.
func NewMessageID() string {
	var b [12]byte
	_, _ = rand.Read(b[:])
	return "msg_" + hex.EncodeToString(b[:])
}

// AnthropicToOpenAI convierte un MessagesRequest a ChatRequest.
func AnthropicToOpenAI(req *anthropic.MessagesRequest, targetModel string) (*openai.ChatRequest, error) {
	if req == nil {
		return nil, fmt.Errorf("petición nil")
	}
	out := &openai.ChatRequest{
		Model:  targetModel,
		Stream: req.Stream,
	}

	if sys := req.System.Text(); sys != "" {
		out.Messages = append(out.Messages, openai.ChatMessage{
			Role:    "system",
			Content: sys,
		})
	}

	for i, m := range req.Messages {
		msgs, err := convertMessage(m)
		if err != nil {
			return nil, fmt.Errorf("messages[%d]: %w", i, err)
		}
		out.Messages = append(out.Messages, msgs...)
	}

	if req.MaxTokens > 0 {
		m := req.MaxTokens
		out.MaxTokens = &m
	}
	if req.Temperature != nil {
		out.Temperature = req.Temperature
	}
	if req.TopP != nil {
		out.TopP = req.TopP
	}
	if len(req.StopSequences) > 0 {
		out.Stop = req.StopSequences
	}
	if req.Stream {
		out.StreamOptions = &openai.StreamOptions{IncludeUsage: true}
	}

	if len(req.Tools) > 0 {
		tools := make([]openai.Tool, 0, len(req.Tools))
		for _, t := range req.Tools {
			params := t.InputSchema
			if params == nil {
				params = map[string]any{"type": "object"}
			}
			tools = append(tools, openai.Tool{
				Type: "function",
				Function: openai.Function{
					Name:        t.Name,
					Description: t.Description,
					Parameters:  params,
				},
			})
		}
		out.Tools = tools
	}
	if req.ToolChoice != nil {
		out.ToolChoice = convertToolChoice(req.ToolChoice)
	}
	return out, nil
}

func convertToolChoice(tc *anthropic.ToolChoice) any {
	switch tc.Type {
	case "auto":
		return "auto"
	case "any":
		return "required"
	case "none":
		return "none"
	case "tool":
		if tc.Name != "" {
			return map[string]any{
				"type":     "function",
				"function": map[string]any{"name": tc.Name},
			}
		}
		return "required"
	default:
		return "auto"
	}
}

// convertMessage convierte un turno Anthropic en 1..N mensajes OpenAI.
// Un turno user con texto + tool_result se descompone en un mensaje user
// más un mensaje tool por cada tool_result (OpenAI lo exige separado).
func convertMessage(m anthropic.Message) ([]openai.ChatMessage, error) {
	switch m.Role {
	case "system":
		// Mensajes de sistema a mitad de conversación (Claude Code los usa).
		var text strings.Builder
		for _, b := range m.Content {
			if b.Type == "text" {
				text.WriteString(b.Text)
			}
		}
		return []openai.ChatMessage{{Role: "system", Content: text.String()}}, nil
	case "user":
		var textParts []openai.Part
		var out []openai.ChatMessage
		flushText := func() {
			if len(textParts) == 0 {
				return
			}
			out = append(out, openai.ChatMessage{Role: "user", Content: textParts})
			textParts = nil
		}
		for _, b := range m.Content {
			switch b.Type {
			case "text":
				textParts = append(textParts, openai.Part{Type: "text", Text: b.Text})
			case "image":
				part, err := convertImage(b)
				if err != nil {
					return nil, err
				}
				textParts = append(textParts, part)
			case "tool_result":
				flushText()
				content := b.Content.Text()
				out = append(out, openai.ChatMessage{
					Role:       "tool",
					Content:    content,
					ToolCallID: b.ToolUseID,
				})
			default:
				textParts = append(textParts, openai.Part{Type: "text", Text: b.Text})
			}
		}
		flushText()
		if len(out) == 0 {
			out = append(out, openai.ChatMessage{Role: "user", Content: ""})
		}
		// Si solo hay un mensaje de texto simple, colapsar a string.
		if len(out) == 1 && out[0].Role == "user" {
			if parts, ok := out[0].Content.([]openai.Part); ok && len(parts) == 1 && parts[0].Type == "text" {
				out[0].Content = parts[0].Text
			}
		}
		return out, nil
	case "assistant":
		var text strings.Builder
		var toolCalls []openai.ToolCall
		for _, b := range m.Content {
			switch b.Type {
			case "text":
				text.WriteString(b.Text)
			case "tool_use":
				args := string(b.Input)
				if args == "" {
					args = "{}"
				}
				toolCalls = append(toolCalls, openai.ToolCall{
					ID:   b.ID,
					Type: "function",
					Function: openai.ToolCallFunction{
						Name:      b.Name,
						Arguments: args,
					},
				})
			case "image":
				// Las imágenes de assistant no tienen equivalente: se describen.
				text.WriteString("[imagen]")
			default:
				text.WriteString(b.Text)
			}
		}
		msg := openai.ChatMessage{Role: "assistant"}
		if s := text.String(); s != "" {
			msg.Content = s
		} else if len(toolCalls) == 0 {
			msg.Content = ""
		} else {
			msg.Content = nil
		}
		if len(toolCalls) > 0 {
			msg.ToolCalls = toolCalls
		}
		return []openai.ChatMessage{msg}, nil
	default:
		return nil, fmt.Errorf("role %q no soportado (user|assistant)", m.Role)
	}
}

func convertImage(b anthropic.Block) (openai.Part, error) {
	if b.Source == nil {
		return openai.Part{}, fmt.Errorf("bloque image sin source")
	}
	switch b.Source.Type {
	case "base64":
		if b.Source.Data == "" || b.Source.MediaType == "" {
			return openai.Part{}, fmt.Errorf("image base64 requiere media_type y data")
		}
		url := "data:" + b.Source.MediaType + ";base64," + b.Source.Data
		return openai.Part{Type: "image_url", ImageURL: &openai.ImageURL{URL: url}}, nil
	case "url":
		if b.Source.URL == "" {
			return openai.Part{}, fmt.Errorf("image url vacía")
		}
		return openai.Part{Type: "image_url", ImageURL: &openai.ImageURL{URL: b.Source.URL}}, nil
	default:
		return openai.Part{}, fmt.Errorf("image source type %q no soportado", b.Source.Type)
	}
}

// OpenAIToAnthropic convierte una respuesta no-stream de OpenAI al formato Anthropic.
func OpenAIToAnthropic(resp *openai.ChatResponse, origModel string) (*anthropic.MessagesResponse, error) {
	if resp == nil || len(resp.Choices) == 0 {
		return nil, fmt.Errorf("respuesta OpenAI vacía")
	}
	ch := resp.Choices[0]
	content := make([]any, 0, 2)

	text := ""
	switch c := ch.Message.Content.(type) {
	case nil:
	case string:
		text = c
	default:
		// Si el proveedor devolvió multipart, extraer texto.
		if parts, ok := asParts(c); ok {
			for _, p := range parts {
				if p.Type == "text" {
					text += p.Text
				}
			}
		} else {
			b, _ := json.Marshal(c)
			text = string(b)
		}
	}
	if text != "" {
		content = append(content, map[string]any{"type": "text", "text": text})
	}
	for _, tc := range ch.Message.ToolCalls {
		var input any = map[string]any{}
		if tc.Function.Arguments != "" {
			var v any
			if err := json.Unmarshal([]byte(tc.Function.Arguments), &v); err == nil {
				input = v
			}
		}
		content = append(content, map[string]any{
			"type":  "tool_use",
			"id":    tc.ID,
			"name":  tc.Function.Name,
			"input": input,
		})
	}
	if len(content) == 0 {
		content = append(content, map[string]any{"type": "text", "text": ""})
	}

	stop := openAIStopReason(ch.FinishReason)
	usage := anthropic.Usage{}
	if resp.Usage != nil {
		usage.InputTokens = resp.Usage.PromptTokens
		usage.OutputTokens = resp.Usage.CompletionTokens
	}
	return &anthropic.MessagesResponse{
		ID:         NewMessageID(),
		Type:       "message",
		Role:       "assistant",
		Content:    content,
		Model:      origModel,
		StopReason: stop,
		Usage:      usage,
	}, nil
}

func asParts(v any) ([]openai.Part, bool) {
	b, err := json.Marshal(v)
	if err != nil {
		return nil, false
	}
	var parts []openai.Part
	if err := json.Unmarshal(b, &parts); err != nil {
		return nil, false
	}
	return parts, true
}

func openAIStopReason(fr *string) *string {
	s := "end_turn"
	if fr != nil {
		switch *fr {
		case "tool_calls":
			s = "tool_use"
		case "length":
			s = "max_tokens"
		case "stop":
			s = "end_turn"
		default:
			s = "end_turn"
		}
	}
	return &s
}
