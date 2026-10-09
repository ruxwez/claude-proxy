// Package responses define los tipos mínimos de OpenAI Responses API
// (POST /responses) necesarios para el adaptador opencode-responses.
// Referencia: Open Responses spec (openresponses.org) y docs de OpenAI.
package responses

// Request es POST {base_url}/responses.
type Request struct {
	Model           string   `json:"model"`
	Input           []any    `json:"input"`
	MaxOutputTokens *int     `json:"max_output_tokens,omitempty"`
	Temperature     *float64 `json:"temperature,omitempty"`
	TopP            *float64 `json:"top_p,omitempty"`
	Stream          bool     `json:"stream,omitempty"`
	Tools           []Tool   `json:"tools,omitempty"`
	ToolChoice      any      `json:"tool_choice,omitempty"`
	Store           *bool    `json:"store,omitempty"`
}

// Tool es una function tool de Responses API.
type Tool struct {
	Type        string         `json:"type"`
	Name        string         `json:"name"`
	Description string         `json:"description,omitempty"`
	Parameters  map[string]any `json:"parameters,omitempty"`
}

// Response es la respuesta no-stream.
type Response struct {
	ID                string             `json:"id"`
	Model             string             `json:"model"`
	Status            string             `json:"status"`
	Output            []OutputItem       `json:"output"`
	Usage             *Usage             `json:"usage,omitempty"`
	IncompleteDetails *IncompleteDetails `json:"incomplete_details,omitempty"`
}

type IncompleteDetails struct {
	Reason string `json:"reason"`
}

// OutputItem es un item de output (message | function_call | reasoning | ...).
type OutputItem struct {
	Type      string        `json:"type"`
	ID        string        `json:"id,omitempty"`
	Role      string        `json:"role,omitempty"`
	Status    string        `json:"status,omitempty"`
	Content   []ContentPart `json:"content,omitempty"`
	CallID    string        `json:"call_id,omitempty"`
	Name      string        `json:"name,omitempty"`
	Arguments string        `json:"arguments,omitempty"`
}

type ContentPart struct {
	Type        string `json:"type"`
	Text        string `json:"text,omitempty"`
	Annotations []any  `json:"annotations,omitempty"`
}

type Usage struct {
	InputTokens  int `json:"input_tokens"`
	OutputTokens int `json:"output_tokens"`
	TotalTokens  int `json:"total_tokens"`
}

// StreamEvent es un evento SSE de Responses API (data: {...}).
// Solo se modelan los campos usados por la traducción.
type StreamEvent struct {
	Type string `json:"type"`
	// deltas
	Delta       string `json:"delta,omitempty"`
	ItemID      string `json:"item_id,omitempty"`
	OutputIndex *int   `json:"output_index,omitempty"`
	// output_item.added / .done
	Item *OutputItem `json:"item,omitempty"`
	// response.completed / .incomplete / .failed
	Response *Response `json:"response,omitempty"`
	// error
	Error *ErrorDetail `json:"error,omitempty"`
}

type ErrorDetail struct {
	Message string `json:"message"`
	Type    string `json:"type,omitempty"`
	Code    string `json:"code,omitempty"`
}
