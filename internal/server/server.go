// Package server expone la API estilo Claude (Anthropic Messages) y enruta
// cada petición al conector indicado por el prefijo del modelo.
//
//	POST /v1/messages
//	POST /v1/messages/count_tokens
//	GET  /v1/models
//	GET  /v1/models/{model_id}
//	GET  /health
package server

import (
	"context"
	"crypto/subtle"
	"encoding/json"
	"io"
	"log/slog"
	"net/http"
	"sort"
	"strconv"
	"strings"
	"time"

	"github.com/ruxwez/claude-proxy/internal/anthropic"
	"github.com/ruxwez/claude-proxy/internal/config"
	"github.com/ruxwez/claude-proxy/internal/connector"
)

// Server es el handler HTTP Claude Like.
type Server struct {
	cfg *config.Settings
	reg *connector.Registry
	log *slog.Logger
	mux *http.ServeMux
}

// New construye el servidor con todas las rutas.
func New(cfg config.Settings, reg *connector.Registry, log *slog.Logger) *Server {
	if log == nil {
		log = slog.Default()
	}
	s := &Server{cfg: &cfg, reg: reg, log: log, mux: http.NewServeMux()}
	s.mux.HandleFunc("POST /v1/messages", s.handleMessages)
	s.mux.HandleFunc("POST /v1/messages/count_tokens", s.handleCountTokens)
	s.mux.HandleFunc("GET /v1/models", s.handleModels)
	s.mux.HandleFunc("GET /v1/models/{model_id...}", s.handleModel)
	s.mux.HandleFunc("GET /health", s.handleHealth)
	return s
}

// ServeHTTP aplica la autenticación opcional y registra cada petición.
func (s *Server) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	start := time.Now()
	if token := strings.TrimSpace(s.cfg.AuthToken); token != "" && !authorized(r, token) {
		writeErr(w, http.StatusUnauthorized, "authentication_error", "token del proxy inválido")
		return
	}
	s.mux.ServeHTTP(w, r)
	s.log.Info("request",
		"method", r.Method, "path", r.URL.Path,
		"dur_ms", time.Since(start).Milliseconds(),
	)
}

func authorized(r *http.Request, token string) bool {
	got := strings.TrimSpace(r.Header.Get("x-api-key"))
	if auth := r.Header.Get("authorization"); strings.HasPrefix(strings.ToLower(auth), "bearer ") {
		if got == "" {
			got = strings.TrimSpace(auth[7:])
		}
	}
	return subtle.ConstantTimeCompare([]byte(got), []byte(token)) == 1
}

func (s *Server) handleHealth(w http.ResponseWriter, _ *http.Request) {
	writeJSON(w, http.StatusOK, map[string]any{
		"status":     "ok",
		"connectors": s.reg.Names(),
	})
}

// handleMessages valida, resuelve el conector por prefijo y delega en él.
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
		writeErr(w, http.StatusBadRequest, "invalid_request_error", `"max_tokens" debe ser > 0`)
		return
	}
	if len(req.Messages) == 0 {
		writeErr(w, http.StatusBadRequest, "invalid_request_error", `"messages" no puede estar vacío`)
		return
	}

	connName, targetModel, ok := splitModel(req.Model)
	if !ok {
		writeErr(w, http.StatusNotFound, "not_found_error",
			"modelo inválido: usa el formato \"conector/modelo\", p. ej. \"opencode-zen/exo-free\". "+
				"Conectores activos: "+strings.Join(s.reg.Names(), ", "))
		return
	}
	c, ok := s.reg.Get(connName)
	if !ok {
		writeErr(w, http.StatusNotFound, "not_found_error",
			"conector "+strconv.Quote(connName)+" no activo. Activos: "+strings.Join(s.reg.Names(), ", "))
		return
	}

	if err := c.Chat(connector.ContextWithRaw(r.Context(), raw), &req, targetModel, w, r); err != nil {
		s.log.Error("conector error", "connector", connName, "model", req.Model, "err", err)
		if w.Header().Get("Content-Type") != "" {
			return // ya se escribió parte de la respuesta
		}
		writeErr(w, http.StatusBadGateway, "api_error", err.Error())
		return
	}
}

// handleModels agrega los modelos de todos los conectores en formato Anthropic.
func (s *Server) handleModels(w http.ResponseWriter, r *http.Request) {
	ctx, cancel := context.WithTimeout(r.Context(), 30*time.Second)
	defer cancel()

	infos := make([]anthropic.ModelInfo, 0, 64)
	for _, c := range s.reg.Connectors() {
		models, err := c.Models(ctx)
		if err != nil {
			s.log.Warn("no se pudieron listar modelos", "connector", c.Name(), "err", err)
			continue
		}
		for _, m := range models {
			infos = append(infos, toModelInfo(c.Name(), m))
		}
	}
	sort.Slice(infos, func(i, j int) bool { return infos[i].ID < infos[j].ID })

	limit := queryInt(r, "limit", 0)
	hasMore := false
	if limit > 0 && len(infos) > limit {
		infos = infos[:limit]
		hasMore = true
	}

	resp := anthropic.ModelsResponse{Data: infos, HasMore: hasMore}
	if len(infos) > 0 {
		first, last := infos[0].ID, infos[len(infos)-1].ID
		resp.FirstID = &first
		resp.LastID = &last
	}
	writeJSON(w, http.StatusOK, resp)
}

// handleModel devuelve un único modelo por su ID completo ("conector/modelo").
func (s *Server) handleModel(w http.ResponseWriter, r *http.Request) {
	id := r.PathValue("model_id")
	connName, raw, ok := splitModel(id)
	if !ok {
		writeErr(w, http.StatusNotFound, "not_found_error", "modelo inválido: "+id)
		return
	}
	c, ok := s.reg.Get(connName)
	if !ok {
		writeErr(w, http.StatusNotFound, "not_found_error", "conector no activo: "+connName)
		return
	}
	ctx, cancel := context.WithTimeout(r.Context(), 30*time.Second)
	defer cancel()
	models, err := c.Models(ctx)
	if err != nil {
		writeErr(w, http.StatusBadGateway, "api_error", err.Error())
		return
	}
	for _, m := range models {
		if m.ID == raw {
			writeJSON(w, http.StatusOK, toModelInfo(connName, m))
			return
		}
	}
	writeErr(w, http.StatusNotFound, "not_found_error", "modelo no encontrado: "+id)
}

func toModelInfo(connName string, m connector.Model) anthropic.ModelInfo {
	display := m.DisplayName
	if display == "" {
		display = m.ID
	}
	created := time.Unix(m.Created, 0).UTC()
	if m.Created == 0 {
		created = time.Unix(0, 0).UTC()
	}
	return anthropic.ModelInfo{
		Type:        "model",
		ID:          connName + "/" + m.ID,
		DisplayName: display,
		CreatedAt:   created.Format(time.RFC3339),
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
	writeJSON(w, http.StatusOK, anthropic.CountTokensResponse{InputTokens: tokens})
}

// splitModel separa "conector/modelo" en sus dos partes.
func splitModel(model string) (connName, raw string, ok bool) {
	name, rest, found := strings.Cut(strings.TrimSpace(model), "/")
	if !found || name == "" || strings.TrimSpace(rest) == "" {
		return "", "", false
	}
	return name, strings.TrimSpace(rest), true
}

func queryInt(r *http.Request, key string, def int) int {
	v := r.URL.Query().Get(key)
	if v == "" {
		return def
	}
	n, err := strconv.Atoi(v)
	if err != nil {
		return def
	}
	return n
}

func writeJSON(w http.ResponseWriter, status int, v any) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	_ = json.NewEncoder(w).Encode(v)
}

func writeErr(w http.ResponseWriter, status int, typ, msg string) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	_ = json.NewEncoder(w).Encode(anthropic.NewError(typ, msg))
}
