// Package opencode implementa el conector para OpenCode Zen y OpenCode Go.
//
// Ambos comparten la misma API key (OPENCODE_API_KEY) y hablan el estándar
// OpenAI. La diferencia es la base URL y el filtrado de modelos:
//
//   - Zen  (https://opencode.ai/zen/v1): solo expone modelos gratuitos
//     (IDs que contienen "free").
//   - Go   (https://opencode.ai/zen/go/v1): expone todo el catálogo.
//
// Los modelos de tipo "muse-spark" solo se sirven por la Responses API,
// así que se enrutan a /responses; el resto usa /chat/completions.
//
// OpenCode solo sirve el tier gratuito (Zen) a peticiones que parecen venir
// de su CLI, así que el conector replica su huella exacta (rama 2.0 de
// packages/opencode/src/session/llm.ts): User-Agent opencode/<versión>,
// x-opencode-client, x-opencode-session e x-opencode-request, con IDs de
// sesión y mensaje en el formato nativo de OpenCode (ses_/msg_).
package opencode

import (
	"context"
	"crypto/rand"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"sort"
	"strings"
	"sync"
	"time"

	"github.com/ruxwez/claude-proxy/internal/anthropic"
	"github.com/ruxwez/claude-proxy/internal/config"
	"github.com/ruxwez/claude-proxy/internal/connector"
	oai "github.com/ruxwez/claude-proxy/internal/openai"
	"github.com/ruxwez/claude-proxy/internal/responses"
	"github.com/ruxwez/claude-proxy/internal/translate"
)

// modelCacheTTL es cuánto se reutiliza la lista de modelos entre llamadas.
const modelCacheTTL = 5 * time.Minute

// Connector es un conector OpenCode (Zen o Go).
type Connector struct {
	name       string
	baseURL    string
	apiKey     string
	userAgent  string
	projectID  string
	filterFree bool
	client     *http.Client
	cache      *connector.ModelCache
}

// opencodeClient es el valor de x-opencode-client: el nombre del cliente que
// envía el CLI (Flag.OPENCODE_CLIENT; "acp" para el adaptador ACP).
const opencodeClient = "cli"

// userAgentSuffix completa el User-Agent con las etiquetas de SDK que envía
// el CLI real de OpenCode (AI SDK sobre Bun). No son obligatorias para la
// API, pero hacen la huella idéntica a la del cliente oficial.
const userAgentSuffix = " ai-sdk/provider-utils/4.0.23 runtime/bun/1.4.0"

// clientUserAgent compone el User-Agent del CLI de OpenCode:
// "opencode/<versión> ai-sdk/... runtime/bun/...".
//
// La API de OpenCode exige este formato (versión >= 1.17.0) para servir el
// tier gratuito; sin él responde 403 FreeTierError ("OpenCode's free tier
// can only be used from within OpenCode"). Vacío = versión por defecto.
func clientUserAgent(version string) string {
	v := strings.TrimSpace(version)
	if v == "" {
		v = config.DefaultClientVersion
	}
	return "opencode/" + v + userAgentSuffix
}

// projectIDOrGenerated usa el project ID configurado o, si está vacío, genera
// uno estable por proceso: OpenCode siempre envía x-opencode-project con el ID
// del proyecto (la API solo lo usa para agrupar, no lo valida).
func projectIDOrGenerated(id string) string {
	if id = strings.TrimSpace(id); id != "" {
		return id
	}
	var b [20]byte
	if _, err := rand.Read(b[:]); err != nil {
		return "global"
	}
	return hex.EncodeToString(b[:])
}

// NewZen crea el conector de OpenCode Zen (solo modelos gratuitos).
// clientVersion es la versión de CLI anunciada en el User-Agent y projectID
// el valor de x-opencode-project (vacío = uno generado).
func NewZen(apiKey, clientVersion, projectID string, timeout time.Duration) *Connector {
	return newConnector("opencode-zen", config.ZenBaseURL, apiKey, clientVersion, projectID, true, timeout)
}

// NewGo crea el conector de OpenCode Go (catálogo completo).
// clientVersion es la versión de CLI anunciada en el User-Agent y projectID
// el valor de x-opencode-project (vacío = uno generado).
func NewGo(apiKey, clientVersion, projectID string, timeout time.Duration) *Connector {
	return newConnector("opencode-go", config.GoBaseURL, apiKey, clientVersion, projectID, false, timeout)
}

func newConnector(name, baseURL, apiKey, clientVersion, projectID string, filterFree bool, timeout time.Duration) *Connector {
	if timeout <= 0 {
		timeout = 120 * time.Second
	}
	return &Connector{
		name:       name,
		baseURL:    strings.TrimRight(baseURL, "/"),
		apiKey:     apiKey,
		userAgent:  clientUserAgent(clientVersion),
		projectID:  projectIDOrGenerated(projectID),
		filterFree: filterFree,
		client:     &http.Client{Timeout: timeout},
		cache:      connector.NewModelCache(modelCacheTTL),
	}
}

// Name devuelve el prefijo del adaptador ("opencode-zen" u "opencode-go").
func (c *Connector) Name() string { return c.name }

// Models lista los modelos del proveedor (con caché de 5 minutos).
func (c *Connector) Models(ctx context.Context) ([]connector.Model, error) {
	if models, ok := c.cache.Get(); ok {
		return models, nil
	}
	models, err := c.fetchModels(ctx)
	if err != nil {
		return nil, err
	}
	c.cache.Set(models)
	return models, nil
}

func (c *Connector) fetchModels(ctx context.Context) ([]connector.Model, error) {
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, c.baseURL+"/models", nil)
	if err != nil {
		return nil, err
	}
	if c.apiKey != "" {
		req.Header.Set("Authorization", "Bearer "+c.apiKey)
	}
	req.Header.Set("Accept", "application/json")
	// Huella del CLI de OpenCode (misma validación que en Chat).
	req.Header.Set("User-Agent", c.userAgent)
	req.Header.Set("x-opencode-client", opencodeClient)

	resp, err := c.client.Do(req)
	if err != nil {
		return nil, fmt.Errorf("%s: no se pudo listar modelos: %w", c.name, err)
	}
	defer resp.Body.Close()
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return nil, fmt.Errorf("%s: /models devolvió %s", c.name, resp.Status)
	}

	var parsed struct {
		Data []struct {
			ID      string `json:"id"`
			Created int64  `json:"created"`
		} `json:"data"`
	}
	if err := json.NewDecoder(resp.Body).Decode(&parsed); err != nil {
		return nil, fmt.Errorf("%s: respuesta /models inválida: %w", c.name, err)
	}

	out := make([]connector.Model, 0, len(parsed.Data))
	for _, m := range parsed.Data {
		id := strings.TrimSpace(m.ID)
		if id == "" {
			continue
		}
		if c.filterFree && !strings.Contains(strings.ToLower(id), "free") {
			continue
		}
		out = append(out, connector.Model{
			ID:          id,
			DisplayName: id,
			Created:     m.Created,
		})
	}
	sort.Slice(out, func(i, j int) bool { return out[i].ID < out[j].ID })
	return out, nil
}

// protocol es el estándar que habla un modelo en OpenCode.
type protocol string

const (
	protoChat      protocol = "chat"      // OpenAI Chat Completions
	protoMessages  protocol = "messages"  // Anthropic Messages nativo
	protoResponses protocol = "responses" // OpenAI Responses API
)

// Chat traduce la petición y la envía al proveedor por el protocolo adecuado.
func (c *Connector) Chat(ctx context.Context, req *anthropic.MessagesRequest, targetModel string, w http.ResponseWriter, r *http.Request) error {
	// Huella del CLI de OpenCode, tal y como la envía
	// packages/opencode/src/session/llm.ts para los proveedores "opencode":
	// el User-Agent opencode/<versión> más las cuatro cabeceras
	// x-opencode-*. Sin ellas la API responde 403 FreeTierError
	// ("OpenCode's free tier can only be used from within OpenCode").
	headers := map[string]string{
		"User-Agent":         c.userAgent,
		"x-opencode-project": c.projectID,
		"x-opencode-session": sessionID(r),
		"x-opencode-request": newMessageID(),
		"x-opencode-client":  opencodeClient,
	}
	if native := r.Header.Get("X-Claude-Code-Session-Id"); native != "" {
		headers["X-Claude-Code-Session-Id"] = native
	}

	switch preferredProtocol(targetModel) {
	case protoMessages:
		// Passthrough nativo: reenviamos el body Anthropic tal cual.
		if v := r.Header.Get("anthropic-version"); v != "" {
			headers["anthropic-version"] = v
		} else {
			headers["anthropic-version"] = "2023-06-01"
		}
		return c.chatMessages(ctx, req, targetModel, w, headers)
	case protoResponses:
		return c.chatResponses(ctx, req, targetModel, w, headers)
	default:
		return c.chatCompletions(ctx, req, targetModel, w, headers)
	}
}

// preferredProtocol indica el protocolo recomendado para cada modelo.
// Según https://opencode.ai/docs/go/#endpoints: algunos solo hablan
// /responses (grok, gpt-luna, muse-spark), otros /messages nativo
// (claude-haiku, minimax, qwen3.7/3.8) y el resto /chat/completions.
func preferredProtocol(model string) protocol {
	id := strings.ToLower(strings.TrimSpace(model))
	switch {
	case strings.Contains(id, "muse-spark"),
		strings.HasPrefix(id, "grok-4.6"), strings.HasPrefix(id, "grok-4.7"),
		strings.HasPrefix(id, "gpt-6-luna"), strings.HasPrefix(id, "gpt-5.6-luna"):
		return protoResponses
	case strings.HasPrefix(id, "claude-haiku-5-5"),
		strings.HasPrefix(id, "minimax-m"),
		strings.HasPrefix(id, "qwen3.7-plus"),
		strings.HasPrefix(id, "qwen3.8-max"), strings.HasPrefix(id, "qwen3.8-flash"):
		return protoMessages
	default:
		return protoChat
	}
}

// --- IDs en formato OpenCode ------------------------------------------------
//
// La API de Zen/Go solo acepta IDs con el formato nativo de OpenCode
// (packages/opencode/src/id/id.ts):
//
//	<prefijo>_ + 12 hex + 14 base62
//
// Los 12 hex son 6 bytes big-endian con los 48 bits bajos de
// ms*4096 + counter. Los IDs de sesión ("ses_", Identifier.descending)
// llevan los bits invertidos (~) para ordenar descendente; los de mensaje
// ("msg_", Identifier.ascending) van tal cual. Cualquier otro formato (el
// UUID de Claude Code, X-Session-Id, o la ausencia total) se rechaza con
// 403, así que se convierten o se generan con este formato.
const (
	idPrefixSession = "ses_"
	idPrefixMessage = "msg_"
	idHexLen        = 12
	idRandLen       = 14
	idBase62        = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz"
	idTotalLen      = len(idPrefixSession) + idHexLen + idRandLen // "ses_" + 12 + 14 = 30
	maxDerived      = 4096                                         // tope del mapa de sesiones derivadas
)

// idState replica el contador monótono de Identifier.create: dos IDs
// generados en el mismo milisegundo se diferencian por el counter.
var (
	idMu     sync.Mutex
	idLastMs int64
	idCount  uint32
)

// derivedMu protege derivedIDs, el mapeo estable cliente → OpenCode para que
// una misma sesión del cliente siempre resuene en la misma sesión del
// proveedor (enrutado y prompt cache).
var (
	derivedMu  sync.Mutex
	derivedIDs = map[string]string{}
)

// sessionID obtiene la sesión del cliente en formato OpenCode: si el cliente
// ya envía un ID con el formato nativo se reenvía tal cual; cualquier otro
// valor (p. ej. el UUID X-Claude-Code-Session-Id de Claude Code) se mapea de
// forma estable al formato de OpenCode; si no llega ninguna cabecera, se
// genera una nueva.
func sessionID(r *http.Request) string {
	for _, h := range []string{"x-opencode-session", "X-Claude-Code-Session-Id", "X-Session-Id"} {
		if v := strings.TrimSpace(r.Header.Get(h)); v != "" {
			if isIdentifier(v, idPrefixSession) {
				return v
			}
			return deriveSessionID(v)
		}
	}
	return newIdentifier(idPrefixSession, true)
}

// newMessageID genera el ID del mensaje de usuario que OpenCode envía en
// x-opencode-request (input.user.id: prefijo msg, ascendente).
func newMessageID() string {
	return newIdentifier(idPrefixMessage, false)
}

// newIdentifier genera un ID nuevo con el formato de OpenCode. Las sesiones
// (descending) llevan el componente temporal invertido, como en el CLI.
func newIdentifier(prefix string, descending bool) string {
	now := time.Now().UnixMilli()
	idMu.Lock()
	if now == idLastMs {
		idCount++
	} else {
		idLastMs = now
		idCount = 0
	}
	counter := idCount
	idMu.Unlock()
	return prefix + idTimeHex(now, counter, descending) + randomBase62(idRandLen)
}

// deriveSessionID mapea un ID de sesión ajeno (p. ej. el UUID de Claude
// Code) a un ID de sesión de OpenCode de forma determinista: la misma
// entrada produce siempre el mismo ID. El hash decide la parte aleatoria; el
// sello de tiempo real mantiene el componente temporal plausible que espera
// la API.
func deriveSessionID(clientID string) string {
	derivedMu.Lock()
	defer derivedMu.Unlock()
	if id, ok := derivedIDs[clientID]; ok {
		return id
	}
	sum := sha256.Sum256([]byte(clientID))
	id := idPrefixSession + idTimeHex(time.Now().UnixMilli(), 0, true) + base62FromBytes(sum[:idRandLen])
	if len(derivedIDs) >= maxDerived {
		derivedIDs = map[string]string{} // reinicio simple: evita crecer sin límite
	}
	derivedIDs[clientID] = id
	return id
}

// isIdentifier comprueba el formato "<prefijo>_" + 12 hex + 14 base62.
func isIdentifier(id, prefix string) bool {
	if len(id) != len(prefix)+idHexLen+idRandLen || !strings.HasPrefix(id, prefix) {
		return false
	}
	for i := len(prefix); i < len(prefix)+idHexLen; i++ {
		if !isHexChar(id[i]) {
			return false
		}
	}
	for i := len(prefix) + idHexLen; i < len(id); i++ {
		if !isBase62Char(id[i]) {
			return false
		}
	}
	return true
}

// isSessionID comprueba el formato de los IDs de sesión de OpenCode.
func isSessionID(id string) bool { return isIdentifier(id, idPrefixSession) }

func isHexChar(c byte) bool {
	return (c >= '0' && c <= '9') || (c >= 'a' && c <= 'f')
}

func isBase62Char(c byte) bool {
	return (c >= '0' && c <= '9') || (c >= 'A' && c <= 'Z') || (c >= 'a' && c <= 'z')
}

// idTimeHex codifica el componente temporal de un ID: current = ms*4096 +
// counter, restringido a sus 48 bits bajos y emitido como 6 bytes big-endian
// en hexadecimal. Con descending se invierten los bits (~current), que es
// como OpenCode genera los IDs de sesión.
func idTimeHex(ms int64, counter uint32, descending bool) string {
	if ms <= 0 {
		ms = time.Now().UnixMilli()
	}
	value := (uint64(ms)<<12 + uint64(counter)) & 0xFFFFFFFFFFFF
	if descending {
		value = ^value & 0xFFFFFFFFFFFF
	}
	const digits = "0123456789abcdef"
	var out [idHexLen]byte
	for i := 0; i < 6; i++ {
		b := byte(value >> (40 - 8*i))
		out[2*i] = digits[b>>4]
		out[2*i+1] = digits[b&0x0f]
	}
	return string(out[:])
}

// randomBase62 devuelve n caracteres del alfabeto base62 de OpenCode.
func randomBase62(n int) string {
	raw := make([]byte, n)
	if _, err := rand.Read(raw); err != nil {
		// crypto/rand no debería fallar; última salida: ruido del reloj.
		seed := uint64(time.Now().UnixNano())
		for i := range raw {
			seed = seed*6364136223846793005 + 1442695040888963407
			raw[i] = byte(seed >> 33)
		}
	}
	return base62FromBytes(raw)
}

// base62FromBytes convierte bytes en caracteres base62 (byte % 62, igual que
// el generador de OpenCode).
func base62FromBytes(raw []byte) string {
	out := make([]byte, len(raw))
	for i, b := range raw {
		out[i] = idBase62[int(b)%len(idBase62)]
	}
	return string(out)
}

// chatMessages reenvía la petición Anthropic tal cual al endpoint /messages
// nativo del proveedor (sin traducción). Es el camino preferido para Claude
// Code porque conserva tool_use, cache_control, thinking, etc.
func (c *Connector) chatMessages(ctx context.Context, req *anthropic.MessagesRequest, targetModel string, w http.ResponseWriter, headers map[string]string) error {
	payload := rewriteModel(ctx, req, targetModel)
	// El endpoint /messages nativo (Anthropic) autentica con x-api-key.
	if c.apiKey != "" {
		headers["x-api-key"] = c.apiKey
	}
	resp, err := connector.PostJSONHeaders(ctx, c.client, c.baseURL+"/messages", payload, headers)
	if err != nil {
		return fmt.Errorf("%s no alcanzable: %w", c.name, err)
	}
	defer resp.Body.Close()

	if ct := resp.Header.Get("Content-Type"); ct != "" {
		w.Header().Set("Content-Type", ct)
	} else {
		w.Header().Set("Content-Type", "application/json")
	}
	w.WriteHeader(resp.StatusCode)
	_, err = io.Copy(w, resp.Body)
	return err
}

// rewriteModel toma el body original (si existe) y sustituye solo el modelo.
func rewriteModel(ctx context.Context, req *anthropic.MessagesRequest, targetModel string) []byte {
	if raw := connector.RawBody(ctx); len(raw) > 0 {
		var m map[string]any
		if err := json.Unmarshal(raw, &m); err == nil {
			m["model"] = targetModel
			if b, err := json.Marshal(m); err == nil {
				return b
			}
		}
	}
	clone := *req
	clone.Model = targetModel
	b, _ := json.Marshal(&clone)
	return b
}

func (c *Connector) chatCompletions(ctx context.Context, req *anthropic.MessagesRequest, targetModel string, w http.ResponseWriter, headers map[string]string) error {
	chatReq, err := translate.AnthropicToOpenAI(req, targetModel)
	if err != nil {
		return err
	}
	payload, err := json.Marshal(chatReq)
	if err != nil {
		return fmt.Errorf("serializar petición %s: %w", c.name, err)
	}
	resp, err := connector.PostJSON(ctx, c.client, c.baseURL+"/chat/completions", c.apiKey, payload, headers)
	if err != nil {
		return fmt.Errorf("%s no alcanzable: %w", c.name, err)
	}
	defer resp.Body.Close()

	if req.Stream {
		if resp.StatusCode < 200 || resp.StatusCode >= 300 {
			return connector.WriteProviderError(w, resp, c.name)
		}
		connector.WriteSSEHeaders(w)
		return translate.OpenAIStreamToAnthropic(resp.Body, w, req.Model)
	}
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return connector.WriteProviderError(w, resp, c.name)
	}
	var chatResp oai.ChatResponse
	if err := json.NewDecoder(resp.Body).Decode(&chatResp); err != nil {
		return fmt.Errorf("decodificar respuesta %s: %w", c.name, err)
	}
	anthResp, err := translate.OpenAIToAnthropic(&chatResp, req.Model)
	if err != nil {
		return err
	}
	return connector.WriteJSON(w, http.StatusOK, anthResp)
}

func (c *Connector) chatResponses(ctx context.Context, req *anthropic.MessagesRequest, targetModel string, w http.ResponseWriter, headers map[string]string) error {
	respReq, err := translate.AnthropicToResponses(req, targetModel)
	if err != nil {
		return err
	}
	payload, err := json.Marshal(respReq)
	if err != nil {
		return fmt.Errorf("serializar petición %s: %w", c.name, err)
	}
	resp, err := connector.PostJSON(ctx, c.client, c.baseURL+"/responses", c.apiKey, payload, headers)
	if err != nil {
		return fmt.Errorf("%s no alcanzable: %w", c.name, err)
	}
	defer resp.Body.Close()

	if req.Stream {
		if resp.StatusCode < 200 || resp.StatusCode >= 300 {
			return connector.WriteProviderError(w, resp, c.name)
		}
		connector.WriteSSEHeaders(w)
		return translate.ResponsesStreamToAnthropic(resp.Body, w, req.Model)
	}
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return connector.WriteProviderError(w, resp, c.name)
	}
	var responsesResp responses.Response
	if err := json.NewDecoder(resp.Body).Decode(&responsesResp); err != nil {
		return fmt.Errorf("decodificar respuesta %s: %w", c.name, err)
	}
	anthResp, err := translate.ResponsesToAnthropic(&responsesResp, req.Model)
	if err != nil {
		return err
	}
	return connector.WriteJSON(w, http.StatusOK, anthResp)
}
