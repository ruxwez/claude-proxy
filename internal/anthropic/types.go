// Package anthropic define los tipos del estándar Anthropic Messages API
// que este proxy acepta como entrada y devuelve como salida.
package anthropic

import (
	"encoding/json"
	"fmt"
)

// MessagesRequest es POST /v1/messages.
type MessagesRequest struct {
	Model         string         `json:"model"`
	MaxTokens     int            `json:"max_tokens"`
	System        System         `json:"system,omitempty"`
	Messages      []Message      `json:"messages"`
	Temperature   *float64       `json:"temperature,omitempty"`
	TopP          *float64       `json:"top_p,omitempty"`
	TopK          *int           `json:"top_k,omitempty"`
	StopSequences []string       `json:"stop_sequences,omitempty"`
	Stream        bool           `json:"stream,omitempty"`
	Tools         []Tool         `json:"tools,omitempty"`
	ToolChoice    *ToolChoice    `json:"tool_choice,omitempty"`
	Metadata      map[string]any `json:"metadata,omitempty"`
}

// System acepta string o []SystemBlock.
type System []SystemBlock

type SystemBlock struct {
	Type string `json:"type"`
	Text string `json:"text"`
}

// UnmarshalJSON acepta "system": "texto" o "system": [{type,text}].
func (s *System) UnmarshalJSON(data []byte) error {
	if len(data) == 0 || string(data) == "null" {
		*s = nil
		return nil
	}
	if data[0] == '"' {
		var str string
		if err := json.Unmarshal(data, &str); err != nil {
			return err
		}
		if str == "" {
			*s = nil
			return nil
		}
		*s = System{{Type: "text", Text: str}}
		return nil
	}
	var blocks []SystemBlock
	if err := json.Unmarshal(data, &blocks); err != nil {
		return fmt.Errorf("system debe ser string o lista de bloques: %w", err)
	}
	*s = System(blocks)
	return nil
}

// MarshalJSON emite siempre la forma de lista (la string se normaliza).
func (s System) MarshalJSON() ([]byte, error) {
	if s == nil {
		return []byte("null"), nil
	}
	return json.Marshal([]SystemBlock(s))
}

// Text concatena los bloques de sistema.
func (s System) Text() string {
	out := ""
	for i, b := range s {
		if i > 0 {
			out += "\n"
		}
		out += b.Text
	}
	return out
}

// Message es un turno de la conversación.
type Message struct {
	Role    string         `json:"role"`
	Content MessageContent `json:"content"`
}

// MessageContent acepta string o []Block (raw para discriminar por type).
type MessageContent []Block

// UnmarshalJSON acepta "content": "hola" o "content": [...].
func (c *MessageContent) UnmarshalJSON(data []byte) error {
	if len(data) == 0 || string(data) == "null" {
		*c = nil
		return nil
	}
	if data[0] == '"' {
		var str string
		if err := json.Unmarshal(data, &str); err != nil {
			return err
		}
		*c = MessageContent{{Type: "text", Text: str}}
		return nil
	}
	var raw []json.RawMessage
	if err := json.Unmarshal(data, &raw); err != nil {
		return fmt.Errorf("content debe ser string o lista de bloques: %w", err)
	}
	blocks := make(MessageContent, 0, len(raw))
	for _, r := range raw {
		b, err := parseBlock(r)
		if err != nil {
			return err
		}
		blocks = append(blocks, b)
	}
	*c = blocks
	return nil
}

// MarshalJSON emite la lista de bloques.
func (c MessageContent) MarshalJSON() ([]byte, error) {
	if c == nil {
		return []byte("null"), nil
	}
	blocks := make([]any, 0, len(c))
	for _, b := range c {
		blocks = append(blocks, blockToWire(b))
	}
	return json.Marshal(blocks)
}

// Block es cualquier bloque de contenido.
type Block struct {
	Type string
	// text
	Text string
	// image
	Source *ImageSource
	// tool_use
	ID    string
	Name  string
	Input json.RawMessage
	// tool_result
	ToolUseID string
	Content   ToolResultContent
	IsError   *bool
}

type ImageSource struct {
	Type      string `json:"type"`
	MediaType string `json:"media_type,omitempty"`
	Data      string `json:"data,omitempty"`
	URL       string `json:"url,omitempty"`
}

// ToolResultContent acepta string o [] bloques (normalmente text/image).
type ToolResultContent []Block

// UnmarshalJSON acepta string o lista.
func (t *ToolResultContent) UnmarshalJSON(data []byte) error {
	if len(data) == 0 || string(data) == "null" {
		*t = nil
		return nil
	}
	if data[0] == '"' {
		var str string
		if err := json.Unmarshal(data, &str); err != nil {
			return err
		}
		*t = ToolResultContent{{Type: "text", Text: str}}
		return nil
	}
	var raw []json.RawMessage
	if err := json.Unmarshal(data, &raw); err != nil {
		return fmt.Errorf("tool_result.content debe ser string o lista: %w", err)
	}
	out := make(ToolResultContent, 0, len(raw))
	for _, r := range raw {
		b, err := parseBlock(r)
		if err != nil {
			return err
		}
		out = append(out, b)
	}
	*t = out
	return nil
}

func (t ToolResultContent) MarshalJSON() ([]byte, error) {
	if t == nil {
		return []byte("null"), nil
	}
	blocks := make([]any, 0, len(t))
	for _, b := range t {
		blocks = append(blocks, blockToWire(b))
	}
	return json.Marshal(blocks)
}

// Text concatena las partes textuales de un tool_result.
func (t ToolResultContent) Text() string {
	out := ""
	for i, b := range t {
		if b.Type != "text" {
			continue
		}
		if i > 0 && out != "" {
			out += "\n"
		}
		out += b.Text
	}
	return out
}

func parseBlock(raw json.RawMessage) (Block, error) {
	var probe struct {
		Type string `json:"type"`
	}
	if err := json.Unmarshal(raw, &probe); err != nil {
		return Block{}, fmt.Errorf("bloque sin type válido: %w", err)
	}
	switch probe.Type {
	case "text":
		var v struct {
			Type string `json:"type"`
			Text string `json:"text"`
		}
		if err := json.Unmarshal(raw, &v); err != nil {
			return Block{}, err
		}
		return Block{Type: "text", Text: v.Text}, nil
	case "image":
		var v struct {
			Type   string      `json:"type"`
			Source ImageSource `json:"source"`
		}
		if err := json.Unmarshal(raw, &v); err != nil {
			return Block{}, err
		}
		src := v.Source
		return Block{Type: "image", Source: &src}, nil
	case "tool_use":
		var v struct {
			Type  string          `json:"type"`
			ID    string          `json:"id"`
			Name  string          `json:"name"`
			Input json.RawMessage `json:"input"`
		}
		if err := json.Unmarshal(raw, &v); err != nil {
			return Block{}, err
		}
		if len(v.Input) == 0 {
			v.Input = json.RawMessage(`{}`)
		}
		return Block{Type: "tool_use", ID: v.ID, Name: v.Name, Input: v.Input}, nil
	case "tool_result":
		var v struct {
			Type      string            `json:"type"`
			ToolUseID string            `json:"tool_use_id"`
			Content   ToolResultContent `json:"content"`
			IsError   *bool             `json:"is_error"`
		}
		if err := json.Unmarshal(raw, &v); err != nil {
			return Block{}, err
		}
		return Block{Type: "tool_result", ToolUseID: v.ToolUseID, Content: v.Content, IsError: v.IsError}, nil
	default:
		// Bloques no soportados (document, thinking, etc.): degradar a texto
		// para no romper la traducción. Se conserva una nota mínima.
		return Block{Type: "text", Text: "[bloque no soportado: " + probe.Type + "]"}, nil
	}
}

func blockToWire(b Block) any {
	switch b.Type {
	case "text":
		return map[string]any{"type": "text", "text": b.Text}
	case "image":
		src := map[string]any{"type": "base64"}
		if b.Source != nil {
			src = map[string]any{
				"type":       b.Source.Type,
				"media_type": b.Source.MediaType,
				"data":       b.Source.Data,
			}
			if b.Source.URL != "" {
				src["url"] = b.Source.URL
			}
		}
		return map[string]any{"type": "image", "source": src}
	case "tool_use":
		var input any = map[string]any{}
		if len(b.Input) > 0 {
			var v any
			if err := json.Unmarshal(b.Input, &v); err == nil {
				input = v
			}
		}
		return map[string]any{"type": "tool_use", "id": b.ID, "name": b.Name, "input": input}
	case "tool_result":
		var content any
		if b.Content == nil {
			content = ""
		} else {
			parts := make([]any, 0, len(b.Content))
			for _, c := range b.Content {
				parts = append(parts, blockToWire(c))
			}
			content = parts
		}
		m := map[string]any{"type": "tool_result", "tool_use_id": b.ToolUseID, "content": content}
		if b.IsError != nil {
			m["is_error"] = *b.IsError
		}
		return m
	default:
		return map[string]any{"type": "text", "text": b.Text}
	}
}

// Tool es una herramienta declarada por el cliente.
type Tool struct {
	Name        string         `json:"name"`
	Description string         `json:"description,omitempty"`
	InputSchema map[string]any `json:"input_schema,omitempty"`
}

// ToolChoice acepta {"type": ...} con name opcional.
type ToolChoice struct {
	Type string `json:"type"`
	Name string `json:"name,omitempty"`
}

// MessagesResponse es la respuesta no-stream de /v1/messages.
type MessagesResponse struct {
	ID           string  `json:"id"`
	Type         string  `json:"type"`
	Role         string  `json:"role"`
	Content      []any   `json:"content"`
	Model        string  `json:"model"`
	StopReason   *string `json:"stop_reason"`
	StopSequence *string `json:"stop_sequence,omitempty"`
	Usage        Usage   `json:"usage"`
}

type Usage struct {
	InputTokens  int `json:"input_tokens"`
	OutputTokens int `json:"output_tokens"`
}

// CountTokensRequest / Response para /v1/messages/count_tokens.
type CountTokensRequest struct {
	Model    string    `json:"model"`
	System   System    `json:"system,omitempty"`
	Messages []Message `json:"messages"`
	Tools    []Tool    `json:"tools,omitempty"`
}

type CountTokensResponse struct {
	InputTokens int `json:"input_tokens"`
}

// ErrorResponse es el formato de error Anthropic.
type ErrorResponse struct {
	Type  string  `json:"type"`
	Error EDetail `json:"error"`
}

type EDetail struct {
	Type    string `json:"type"`
	Message string `json:"message"`
}

// NewError construye un ErrorResponse.
func NewError(typ, msg string) ErrorResponse {
	return ErrorResponse{Type: "error", Error: EDetail{Type: typ, Message: msg}}
}

// ModelsResponse es GET /v1/models en el formato de Anthropic.
type ModelsResponse struct {
	Data    []ModelInfo `json:"data"`
	HasMore bool        `json:"has_more"`
	FirstID *string     `json:"first_id"`
	LastID  *string     `json:"last_id"`
}

// ModelInfo es un modelo en el formato de Anthropic.
type ModelInfo struct {
	Type        string `json:"type"` // "model"
	ID          string `json:"id"`
	DisplayName string `json:"display_name"`
	CreatedAt   string `json:"created_at"` // RFC3339
}
