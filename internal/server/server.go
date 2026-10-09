package server

import (
	"encoding/json"
	"io"
	"log/slog"
	"net/http"
	"strings"
	"time"

	anthropicadapter "github.com/ruxwez/claude-proxy/internal/adapter/anthropic"
	"github.com/ruxwez/claude-proxy/internal/adapter"
	_ "github.com/ruxwez/claude-proxy/internal/adapter/anthropic"
	_ "github.com/ruxwez/claude-proxy/internal/adapter/openai"
	_ "github.com/ruxwez/claude-proxy/internal/adapter/opencode"
	"github.com/ruxwez/claude-proxy/internal/anthropic"
	"github.com/ruxwez/claude-proxy/internal/config"
)

// Server es el handler HTTP Claude Like.
type Server struct {
	cfg *config.Config
	mux *http.ServeMux
	log *slog.Logger
}

// New construye el mux con todas las rutas.
func New(cfg *config.Config, log *slog.Logger) *Server {
	if log == nil {
		log = slog.Default()
	}
	s := &Server{cfg: cfg, mux: http.NewServeMux(), log: log}
	s.mux.HandleFunc("POST /v1/messages", s.handleMessages)
	s.mux.HandleFunc("POST /v1/messages/count_tokens", s.handleCountTokens)
	s.mux.HandleFunc("GET /v1/models", s.handleModels)
	s.mux.HandleFunc("GET /health", s.handleHealth)
	return s
}

func (s *Server) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	start := time.Now()
	// Auth opcional del proxy.
	if token := strings.TrimSpace(s.cfg.Server.AuthToken); token != "" {
		got := r.Header.Get("x-api-key")
		if auth := r.Header.Get("authorization"); strings.HasPrefix(strings.ToLower(auth), "bearer ") {
			if got == "" {
				got = strings.TrimSpace(auth[7:])
			}
		}
		if got != token {
			writeErr(w, http.StatusUnauthorized, "authentication_error", "token del proxy inválido")
			return
		}
	}
	s.mux.ServeHTTP(w, r)
	s.log.Info("request",
		"method", r.Method, "path", r.URL.Path,
		"dur_ms", time.Since(start).Milliseconds(),
	)
}

func (s *Server) handleHealth(w http.ResponseWriter, _ *http.Request) {
	w.Header().Set("Content-Type", "application/json")
	_, _ = w.Write([]byte(`{"status":"ok"}`))
}

func (s *Server) handleModels(w http.ResponseWriter, _ *http.Request) {
	models := s.cfg.Models()
	data := make([]anthropic.ModelInfo, 0, len(models))
	for _, m := range models {
		data = append(data, anthropic.ModelInfo{
			ID:      m,
			Object:  "model",
			Created: 0,
			OwnedBy: "claude-proxy",
		})
	}
	if s.cfg.Default != nil && s.cfg.Default.TargetModel != "" {
		data = append(data, anthropic.ModelInfo{
			ID:      "default",
			Object:  "model",
			Created: 0,
			OwnedBy: "claude-proxy",
		})
	}
	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(anthropic.ModelsResponse{Data: data})
}

// handleMessages es POST /v1/messages: valida, resuelve ruta y delega al adaptador.
func (s *Server) handleMessages(w http.ResponseWriter, r *http.Request) {
	if ct := r.Header.Get("content-type"); ct != "" && !strings.Contains(ct, "application/json") {
		writeErr(w, http.StatusBadRequest, "invalid_request_error", "content-type debe ser application/json")
		return
	}
	raw, err := io.ReadAll(io.LimitReader(r.Body, 32<<20))
	if err != nil {
		writeErr(w, http.StatusBadRequest, "invalid_request_error", "no se pudo leer el body")
		return
	}
	var req anthropic.MessagesRequest
	if err := json.Unmarshal(raw, &req); err != nil {
		writeErr(w, http.StatusBadRequest, "invalid_request_error", "JSON inválido: "+err.Error())
		return
	}
	if strings.TrimSpace(req.Model) == "" {
		writeErr(w, http.StatusBadRequest, "invalid_request_error", `"model" es requerido`)
		return
	}
	if req.MaxTokens <= 0 && !req.Stream {
		// Anthropic exige max_tokens; en stream algunos SDKs lo omiten al inicio.
		writeErr(w, http.StatusBadRequest, "invalid_request_error", `"max_tokens" debe ser > 0`)
		return
	}
	if len(req.Messages) == 0 {
		writeErr(w, http.StatusBadRequest, "invalid_request_error", `"messages" no puede estar vacío`)
		return
	}

	route, err := s.cfg.Resolve(req.Model)
	if err != nil {
		writeErr(w, http.StatusNotFound, "not_found_error", err.Error())
		return
	}

	a, err := adapter.Get(route.Provider)
	if err != nil {
		writeErr(w, http.StatusBadGateway, "api_error", err.Error())
		return
	}

	ctx := anthropicadapter.ContextWithRaw(r.Context(), raw)
	if err := a.Handle(ctx, &req, route, w, r); err != nil {
		s.log.Error("adapter error", "provider", route.Provider, "model", req.Model, "err", err)
		// Si el adaptador ya escribió cabeceras, no se puede reescribir.
		if w.Header().Get("Content-Type") != "" {
			return
		}
		writeErr(w, http.StatusBadGateway, "api_error", err.Error())
		return
	}
}

// handleCountTokens estima tokens localmente (chars/4) sin llamar al proveedor.
func (s *Server) handleCountTokens(w http.ResponseWriter, r *http.Request) {
	raw, err := io.ReadAll(io.LimitReader(r.Body, 32<<20))
	if err != nil {
		writeErr(w, http.StatusBadRequest, "invalid_request_error", "no se pudo leer el body")
		return
	}
	var req anthropic.CountTokensRequest
	if err := json.Unmarshal(raw, &req); err != nil {
		writeErr(w, http.StatusBadRequest, "invalid_request_error", "JSON inválido: "+err.Error())
		return
	}
	chars := len(req.System.Text())
	for _, m := range req.Messages {
		for _, b := range m.Content {
			chars += len(b.Text)
			if b.Source != nil {
				chars += len(b.Source.Data) / 4
			}
			if len(b.Input) > 0 {
				chars += len(b.Input) / 4
			}
			chars += len(b.Content.Text())
		}
	}
	for _, t := range req.Tools {
		chars += len(t.Name) + len(t.Description)
		if b, err := json.Marshal(t.InputSchema); err == nil {
			chars += len(b) / 4
		}
	}
	tokens := chars / 4
	if tokens < 1 {
		tokens = 1
	}
	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(anthropic.CountTokensResponse{InputTokens: tokens})
}

func writeErr(w http.ResponseWriter, status int, typ, msg string) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	_ = json.NewEncoder(w).Encode(anthropic.NewError(typ, msg))
}
