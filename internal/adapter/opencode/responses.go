package opencode

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"

	"github.com/ruxwez/claude-proxy/internal/anthropic"
	"github.com/ruxwez/claude-proxy/internal/config"
	"github.com/ruxwez/claude-proxy/internal/responses"
	"github.com/ruxwez/claude-proxy/internal/translate"
)

// ResponsesAdapter implementa provider "opencode-responses" vía /responses.
// Cubre muse-spark-1.x-contributor-free (Zen solo lo sirve en Responses API).
type ResponsesAdapter struct{}

func (a *ResponsesAdapter) Provider() string { return "opencode-responses" }

func (a *ResponsesAdapter) Handle(ctx context.Context, anthReq *anthropic.MessagesRequest, route config.ResolvedRoute, w http.ResponseWriter, r *http.Request) error {
	respReq, err := translate.AnthropicToResponses(anthReq, route.TargetModel)
	if err != nil {
		return err
	}
	payload, err := json.Marshal(respReq)
	if err != nil {
		return fmt.Errorf("serializar petición responses: %w", err)
	}
	resp, err := doPost(ctx, route, r, "/responses", payload)
	if err != nil {
		return err
	}
	defer resp.Body.Close()

	if anthReq.Stream {
		if resp.StatusCode < 200 || resp.StatusCode >= 300 {
			return writeProviderError(w, resp, "opencode-responses")
		}
		writeSSEHeaders(w)
		return translate.ResponsesStreamToAnthropic(resp.Body, w, anthReq.Model)
	}
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return writeProviderError(w, resp, "opencode-responses")
	}
	var responsesResp responses.Response
	if err := json.NewDecoder(resp.Body).Decode(&responsesResp); err != nil {
		return fmt.Errorf("decodificar respuesta responses: %w", err)
	}
	anthResp, err := translate.ResponsesToAnthropic(&responsesResp, anthReq.Model)
	if err != nil {
		return err
	}
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(http.StatusOK)
	return json.NewEncoder(w).Encode(anthResp)
}
