// Records server.max-request-body-bytes answers (CLIProxyAPI v8.0.35 dd7b4334) from a
// complete api.NewServer driven in-process through httptest. It opens no sockets and
// calls no provider: no credentials are configured, so accepted bodies end at the
// model lookup.
package main

import (
	"bytes"
	"context"
	"errors"
	"encoding/base64"
	"encoding/json"
	"io"
	"mime/multipart"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"sync/atomic"

	"github.com/gin-gonic/gin"
	"github.com/klauspost/compress/zstd"
	"github.com/router-for-me/CLIProxyAPI/v8/internal/api"
	"github.com/router-for-me/CLIProxyAPI/v8/internal/config"
	"github.com/router-for-me/CLIProxyAPI/v8/internal/pluginhost"
	sdkaccess "github.com/router-for-me/CLIProxyAPI/v8/sdk/access"
	coreauth "github.com/router-for-me/CLIProxyAPI/v8/sdk/cliproxy/auth"
)

type scenario struct {
	Name    string            `json:"name"`
	YAML    string            `json:"yaml"`
	Method  string            `json:"method"`
	Path    string            `json:"path"`
	Headers map[string]string `json:"headers,omitempty"`
	Body    string            `json:"body_b64"`
	// Chunked sends the body without a Content-Length.
	Chunked bool `json:"chunked"`
	// PluginAuth adds a frontend auth provider that reads the body the way Go's plugin
	// host adapter does, after the config keys.
	PluginAuth bool `json:"plugin_auth,omitempty"`

	Status      int    `json:"status"`
	Response    string `json:"response"`
	ContentType string `json:"content_type"`
	// Read is how many body bytes the server took from the client.
	Read int64 `json:"read"`
}

type counter struct {
	r io.Reader
	n atomic.Int64
}

func (c *counter) Read(p []byte) (int, error) {
	n, err := c.r.Read(p)
	c.n.Add(int64(n))
	return n, err
}

// bodyAuth is pluginhost accessAdapter.Authenticate for a plugin that never accepts:
// readAndRestoreRequestBody, then NewRequestEntityTooLargeError on a MaxBytesError.
type bodyAuth struct{}

func (bodyAuth) Identifier() string { return "plugin:test:body-auth" }

func (bodyAuth) Authenticate(_ context.Context, r *http.Request) (*sdkaccess.Result, *sdkaccess.AuthError) {
	if r.Body == nil {
		return nil, sdkaccess.NewNotHandledError()
	}
	orig := r.Body
	body, err := io.ReadAll(orig)
	if err != nil {
		r.Body = struct {
			io.Reader
			io.Closer
		}{io.MultiReader(bytes.NewReader(body), orig), orig}
		var maxBytesErr *http.MaxBytesError
		if errors.As(err, &maxBytesErr) {
			return nil, sdkaccess.NewRequestEntityTooLargeError(err)
		}
		return nil, sdkaccess.NewInternalAuthError("failed to read plugin auth request body", err)
	}
	r.Body = io.NopCloser(bytes.NewReader(body))
	return nil, sdkaccess.NewNotHandledError()
}

func must(err error) {
	if err != nil {
		panic(err)
	}
}

func api64(extra string) string {
	return "config-version: 8\nserver:\n  max-request-body-bytes: 64\naccess:\n  api-keys:\n    - test-key\n" + extra
}

const requestLog = "observability:\n  logs:\n    request-log: true\n"

const mgmtYAML = "config-version: 8\nserver:\n  max-request-body-bytes: 32\nmanagement:\n  allow-remote: true\n  secret-key: mgmt-secret\n"

func zstdBytes(raw string) string {
	var out bytes.Buffer
	enc, err := zstd.NewWriter(&out)
	must(err)
	_, err = enc.Write([]byte(raw))
	must(err)
	must(enc.Close())
	return out.String()
}

func form(fields [][2]string, file string) (string, string) {
	var buf bytes.Buffer
	w := multipart.NewWriter(&buf)
	must(w.SetBoundary("BOUNDARY0123456789"))
	for _, f := range fields {
		must(w.WriteField(f[0], f[1]))
	}
	if file != "" {
		part, err := w.CreateFormFile("file", "upload.json")
		must(err)
		_, err = part.Write([]byte(file))
		must(err)
	}
	must(w.Close())
	return buf.String(), w.FormDataContentType()
}

func scenarios() []scenario {
	big := `{"model":"gpt-5","messages":[{"role":"user","content":"` + strings.Repeat("x", 128) + `"}]}`
	auth := map[string]string{"Authorization": "Bearer test-key", "Content-Type": "application/json"}
	mgmt := map[string]string{"Authorization": "Bearer mgmt-secret", "Content-Type": "application/json"}
	var out []scenario
	add := func(s scenario) { out = append(out, s) }

	add(scenario{Name: "content_length_over_limit_reads_nothing", YAML: api64(requestLog), Method: "POST", Path: "/v1/chat/completions",
		Headers: map[string]string{"Content-Type": "application/json"}, Body: strings.Repeat("a", 128)})
	add(scenario{Name: "content_length_over_limit_authenticated", YAML: api64(""), Method: "POST", Path: "/v1/messages",
		Headers: auth, Body: big})
	add(scenario{Name: "chunked_unauthenticated_request_log", YAML: api64(requestLog), Method: "POST", Path: "/v1/chat/completions",
		Headers: map[string]string{"Content-Type": "application/json"}, Body: strings.Repeat("b", 1024), Chunked: true})
	add(scenario{Name: "chunked_unauthenticated_no_request_log", YAML: api64(""), Method: "POST", Path: "/v1/chat/completions",
		Headers: map[string]string{"Content-Type": "application/json"}, Body: strings.Repeat("b", 1024), Chunked: true})
	add(scenario{Name: "chunked_authenticated_request_log", YAML: api64(requestLog), Method: "POST", Path: "/v1/responses",
		Headers: auth, Body: big, Chunked: true})
	for _, path := range []string{
		"/v1/chat/completions", "/v1/completions", "/v1/responses", "/v1/responses/compact",
		"/backend-api/codex/responses", "/v1/messages", "/v1/messages/count_tokens",
		"/v1beta/models/gemini-2.5-pro:generateContent", "/v1beta/models/gemini-2.5-pro:streamGenerateContent",
		"/v1beta/interactions", "/v1/images/generations", "/v1/images/edits", "/v1/audio/speech", "/v1/tts",
		"/v1/videos", "/v1/videos/generations", "/openai/v1/videos",
	} {
		add(scenario{Name: "chunked_authenticated " + path, YAML: api64(""), Method: "POST", Path: path, Headers: auth, Body: big, Chunked: true})
	}
	body, contentType := form([][2]string{{"model", "gpt-image-1"}, {"prompt", strings.Repeat("p", 128)}}, "")
	for _, path := range []string{"/v1/images/edits", "/openai/v1/videos"} {
		add(scenario{Name: "multipart_chunked " + path, YAML: api64(""), Method: "POST", Path: path,
			Headers: map[string]string{"Authorization": "Bearer test-key", "Content-Type": contentType}, Body: body, Chunked: true})
	}
	add(scenario{Name: "urlencoded_chunked /openai/v1/videos", YAML: api64(""), Method: "POST", Path: "/openai/v1/videos",
		Headers: map[string]string{"Authorization": "Bearer test-key", "Content-Type": "application/x-www-form-urlencoded"},
		Body: "prompt=" + strings.Repeat("p", 128), Chunked: true})

	zstdYAML := "config-version: 8\nserver:\n  max-request-body-bytes: 128\naccess:\n  api-keys:\n    - test-key\n"
	compressed := zstdBytes(`{"model":"gpt-5","input":"` + strings.Repeat("z", 512) + `"}`)
	zstdHeaders := map[string]string{"Authorization": "Bearer test-key", "Content-Type": "application/json", "Content-Encoding": "zstd"}
	for _, path := range []string{"/v1/responses", "/v1/chat/completions", "/v1/audio/speech"} {
		add(scenario{Name: "zstd_decoded_over_limit " + path, YAML: zstdYAML, Method: "POST", Path: path, Headers: zstdHeaders, Body: compressed})
	}
	add(scenario{Name: "zstd_decoded_over_limit_request_log", YAML: zstdYAML + requestLog, Method: "POST", Path: "/v1/responses", Headers: zstdHeaders, Body: compressed})
	for _, path := range []string{"/v1/images/generations", "/v1/images/edits", "/v1/videos", "/v1/videos/generations", "/v1/videos/edits", "/v1/videos/extensions", "/openai/v1/videos"} {
		add(scenario{Name: "zstd_decoded_over_limit " + path, YAML: zstdYAML, Method: "POST", Path: path, Headers: zstdHeaders, Body: compressed})
	}
	exact := zstdBytes(`{"model":"gpt-5","input":"` + strings.Repeat("z", 100) + `"}`)
	add(scenario{Name: "zstd_decoded_at_limit", YAML: zstdYAML, Method: "POST", Path: "/v1/responses", Headers: zstdHeaders, Body: exact})

	exactYAML := "config-version: 8\nserver:\n  max-request-body-bytes: 30\naccess:\n  api-keys:\n    - test-key\n"
	add(scenario{Name: "exact_boundary_accepted", YAML: exactYAML, Method: "POST", Path: "/v1/responses", Headers: auth, Body: `{"model":"gpt-5","input":"ok"}`})
	add(scenario{Name: "exact_boundary_chunked_accepted", YAML: exactYAML, Method: "POST", Path: "/v1/responses", Headers: auth, Body: `{"model":"gpt-5","input":"ok"}`, Chunked: true})
	add(scenario{Name: "one_over_boundary_chunked", YAML: exactYAML, Method: "POST", Path: "/v1/responses", Headers: auth, Body: `{"model":"gpt-5","input":"ok!"}`, Chunked: true})
	negYAML := "config-version: 8\nserver:\n  max-request-body-bytes: -1\naccess:\n  api-keys:\n    - test-key\n"
	add(scenario{Name: "negative_limit_disables", YAML: negYAML, Method: "POST", Path: "/v1/responses", Headers: auth,
		Body: `{"model":"gpt-5","input":"` + strings.Repeat("a", 256) + `"}`, Chunked: true})
	add(scenario{Name: "legacy_root_key", YAML: "max-request-body-bytes: 16\napi-keys:\n  - test-key\n", Method: "POST", Path: "/v1/responses", Headers: auth,
		Body: `{"model":"gpt-5","input":"ok"}`})
	add(scenario{Name: "v8_key_wins_over_legacy", YAML: "max-request-body-bytes: 16\nserver:\n  max-request-body-bytes: 0\napi-keys:\n  - test-key\n", Method: "POST", Path: "/v1/responses", Headers: auth,
		Body: `{"model":"gpt-5","input":"ok"}`})
	add(scenario{Name: "get_with_body_over_limit", YAML: api64(""), Method: "GET", Path: "/v1/models", Headers: auth, Body: big, Chunked: true})
	add(scenario{Name: "healthz_content_length_over_limit", YAML: api64(""), Method: "GET", Path: "/healthz", Body: big})

	oversized := `{"version":"` + strings.Repeat("v", 128) + `"}`
	for _, route := range [][2]string{
		{"PUT", "/v8/management/config"},
		{"PATCH", "/v8/management/config"},
		{"PUT", "/v8/management/config.yaml"},
		{"PUT", "/v8/management/config/server/port"},
		{"POST", "/v8/management/plugins/store/demo/install"},
		{"POST", "/v0/management/plugin-store/demo/install"},
		{"POST", "/v0/management/api-call"},
		{"POST", "/v8/management/requests/api-call"},
		{"POST", "/v0/management/auth-files?name=x.json"},
		{"POST", "/v8/management/credentials?name=x.json"},
		{"DELETE", "/v0/management/auth-files"},
		{"PATCH", "/v0/management/auth-files/status"},
		{"PATCH", "/v8/management/credentials/fields"},
		{"POST", "/v0/management/auth-files/refresh"},
		{"POST", "/v0/management/oauth-callback"},
		{"POST", "/v8/management/oauth/callback"},
		{"POST", "/v0/management/reset-quota"},
		{"POST", "/v8/management/routing/cooldown/reset"},
		{"POST", "/v0/management/plugins/demo/quota"},
		{"POST", "/v0/management/plugins/demo/quota/reset"},
		{"PUT", "/v0/management/debug"},
		{"PUT", "/v0/management/config.yaml"},
		{"PUT", "/v0/management/api-keys"},
	} {
		add(scenario{Name: "management_chunked " + route[0] + " " + route[1], YAML: mgmtYAML, Method: route[0], Path: route[1], Headers: mgmt, Body: oversized, Chunked: true})
	}
	upload, uploadType := form(nil, `{"type":"claude","access_token":"`+strings.Repeat("t", 128)+`"}`)
	for _, path := range []string{"/v0/management/auth-files", "/v0/management/vertex/import", "/v8/management/oauth/import?provider=vertex"} {
		add(scenario{Name: "management_multipart_chunked " + path, YAML: mgmtYAML, Method: "POST", Path: path,
			Headers: map[string]string{"Authorization": "Bearer mgmt-secret", "Content-Type": uploadType}, Body: upload, Chunked: true})
	}
	for _, path := range []string{"/v1/chat/completions", "/v1/realtime/calls", "/v1/live"} {
		add(scenario{Name: "plugin_auth_chunked " + path, YAML: api64(""), Method: "POST", Path: path, PluginAuth: true,
			Headers: map[string]string{"Authorization": "Bearer wrong-key", "Content-Type": "application/json"}, Body: strings.Repeat("p", 1024), Chunked: true})
	}
	add(scenario{Name: "plugin_auth_content_length_over_limit", YAML: api64(""), Method: "POST", Path: "/v1/realtime/calls", PluginAuth: true,
		Headers: map[string]string{"Authorization": "Bearer wrong-key", "Content-Type": "application/json"}, Body: strings.Repeat("p", 1024)})
	for _, path := range []string{"/v1/realtime/client_secrets", "/v1/realtime/sessions", "/v1/realtime/calls", "/v1/live", "/v1/realtime/calls/rtc_1/hangup"} {
		add(scenario{Name: "realtime_chunked " + path, YAML: api64(""), Method: "POST", Path: path, Headers: auth, Body: big, Chunked: true})
	}
	// Go reads the v8 config body only for writes other than DELETE.
	add(scenario{Name: "management_chunked GET /v8/management/config", YAML: mgmtYAML, Method: "GET", Path: "/v8/management/config", Headers: mgmt, Body: strings.Repeat(" ", 33), Chunked: true})
	add(scenario{Name: "management_chunked DELETE /v8/management/config/server/max-request-body-bytes", YAML: mgmtYAML, Method: "DELETE",
		Path: "/v8/management/config/server/max-request-body-bytes", Headers: mgmt, Body: strings.Repeat(" ", 33), Chunked: true})
	// Known differences (docs/DIFFERENCES-FROM-GO.md): Go's JSON binder decodes the first
	// value from the permitted prefix, and Go's multipart reader reports either the limit
	// or a parse error depending on where the limit falls.
	add(scenario{Name: "json_first_value_within_limit /v0/management/plugins/demo/quota", YAML: mgmtYAML, Method: "POST", Path: "/v0/management/plugins/demo/quota",
		Headers: mgmt, Body: "{}" + strings.Repeat(" ", 64), Chunked: true})
	fileForm, fileType := form(nil, `{"type":"claude","access_token":"`+strings.Repeat("t", 2048)+`"}`)
	kiB := strings.Replace(mgmtYAML, "max-request-body-bytes: 32", "max-request-body-bytes: 1024", 1)
	for _, path := range []string{"/v0/management/auth-files", "/v8/management/credentials", "/v0/management/vertex/import", "/v8/management/oauth/import?provider=vertex"} {
		add(scenario{Name: "multipart_file_data_over_limit " + path, YAML: kiB, Method: "POST", Path: path,
			Headers: map[string]string{"Authorization": "Bearer mgmt-secret", "Content-Type": fileType}, Body: fileForm, Chunked: true})
	}
	imageForm, imageType := form([][2]string{{"model", "gpt-image-1"}, {"prompt", "p"}}, "")
	for _, path := range []string{"/v1/images/edits", "/openai/v1/videos"} {
		add(scenario{Name: "multipart_headers_over_limit " + path, YAML: exactYAML, Method: "POST", Path: path,
			Headers: map[string]string{"Authorization": "Bearer test-key", "Content-Type": imageType}, Body: imageForm, Chunked: true})
	}
	add(scenario{Name: "management_content_length_over_limit", YAML: mgmtYAML, Method: "PUT", Path: "/v8/management/config", Headers: mgmt, Body: oversized})
	add(scenario{Name: "management_unauthenticated_chunked", YAML: mgmtYAML, Method: "PUT", Path: "/v8/management/config",
		Headers: map[string]string{"Content-Type": "application/json"}, Body: oversized, Chunked: true})

	// Video form parse errors, which do not depend on the limit.
	noLimit := "config-version: 8\naccess:\n  api-keys:\n    - test-key\n"
	urlencoded := "application/x-www-form-urlencoded"
	videoForm, videoType := form([][2]string{{"prompt", "p"}}, "")
	for _, c := range []struct{ name, path, contentType, body string }{
		{"no_boundary", "/openai/v1/videos", "multipart/form-data", ""},
		{"bad_escape", "/openai/v1/videos", urlencoded, "prompt=p&ignored=%ZZ"},
		{"short_escape", "/openai/v1/videos", urlencoded, "prompt=p&ignored=%Z"},
		{"semicolon", "/openai/v1/videos", urlencoded, "prompt=p&a=%ZZ&b=1;c=2"},
		{"bad_query_urlencoded", "/openai/v1/videos?x=%ZZ", urlencoded, "prompt=p"},
		{"bad_query_multipart", "/openai/v1/videos?x=%ZZ", videoType, videoForm},
		{"bad_media_param", "/openai/v1/videos", urlencoded + "; x", "prompt=p"},
		{"content_after_subtype", "/openai/v1/videos", urlencoded + " x", "prompt=p"},
	} {
		for _, limit := range []struct{ name, yaml string }{{"limit_off", noLimit}, {"limit_1024", strings.Replace(noLimit, "\n", "\nserver:\n  max-request-body-bytes: 1024\n", 1)}} {
			add(scenario{Name: "video_form_" + c.name + " " + limit.name, YAML: limit.yaml, Method: "POST", Path: c.path,
				Headers: map[string]string{"Authorization": "Bearer test-key", "Content-Type": c.contentType}, Body: c.body})
		}
	}
	return out
}

func run(s *scenario, raw string) {
	dir, err := os.MkdirTemp("", "cpa-body-limit-")
	must(err)
	defer os.RemoveAll(dir)
	must(os.Chdir(dir))
	path := filepath.Join(dir, "config.yaml")
	must(os.WriteFile(path, []byte(s.YAML+"auth-dir: "+filepath.Join(dir, "auth")+"\n"), 0o600))
	cfg, err := config.LoadConfig(path)
	must(err)
	access := sdkaccess.NewManager()
	server := api.NewServer(cfg, coreauth.NewManager(nil, nil, nil), access, path, api.WithPluginHost(pluginhost.New()))
	if s.PluginAuth {
		access.SetProviders(append(access.Providers(), bodyAuth{}))
	}
	c := &counter{r: strings.NewReader(raw)}
	req := httptest.NewRequest(s.Method, s.Path, io.NopCloser(c))
	req.RemoteAddr = "192.0.2.1:1234"
	if s.Chunked {
		req.ContentLength = -1
	} else {
		req.ContentLength = int64(len(raw))
	}
	for k, v := range s.Headers {
		req.Header.Set(k, v)
	}
	rec := httptest.NewRecorder()
	server.Handler().ServeHTTP(rec, req)
	s.Status = rec.Code
	s.Response = rec.Body.String()
	s.ContentType = rec.Header().Get("Content-Type")
	s.Read = c.n.Load()
}

func main() {
	gin.SetMode(gin.ReleaseMode)
	cases := scenarios()
	for i := range cases {
		raw := cases[i].Body
		cases[i].Body = base64.StdEncoding.EncodeToString([]byte(raw))
		run(&cases[i], raw)
	}
	data, err := json.MarshalIndent(cases, "", "  ")
	must(err)
	must(os.WriteFile(os.Args[1], append(data, '\n'), 0o644))
}
