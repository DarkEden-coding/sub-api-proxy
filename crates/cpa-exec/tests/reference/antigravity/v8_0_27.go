//go:build v8027

// Scenarios for the Antigravity behaviour ported from CLIProxyAPI v8.0.27. They need the
// v8.0.27 tree, so they build only with `-tags v8027` and run with
// ANTIGRAVITY_GO_SET=v8.0.27 (README.md).
package main

func init() {
	scenarioSets["v8.0.27"] = v8027Scenarios
}

func v8027Scenarios() []execScenario {
	answer := func(model, trace string) string {
		return `{"response":{"candidates":[{"content":{"role":"model","parts":[{"thought":true,"text":"checked limits"},{"text":"34113"}]},"finishReason":"STOP"}],"modelVersion":"` + model + `","usageMetadata":{"promptTokenCount":10,"candidatesTokenCount":2,"totalTokenCount":12}},"traceId":"` + trace + `"}`
	}
	return []execScenario{
		// The Claude 5.5 models of the static catalog (Go e3abd9ae, c575291a, 6b037f61,
		// 8a945b3f): a thinking budget above their 64,000 maximum and an output limit
		// above their 128,000 tokens.
		{Name: "claude55_thinking_budget_above_max", Op: "execute", Source: "claude", Model: "claude-opus-5-5-high",
			Payload:  `{"model":"claude-opus-5-5-high","max_tokens":200000,"thinking":{"type":"enabled","budget_tokens":100000},"messages":[{"role":"user","content":"limits"}]}`,
			Upstream: sseOK(answer("claude-opus-5-5-high", "t-limits-1"))},
		{Name: "claude55_openai_reasoning_effort", Op: "execute", Source: "openai", Model: "claude-sonnet-5-5-high",
			Payload:  `{"model":"claude-sonnet-5-5-high","reasoning_effort":"high","messages":[{"role":"user","content":"effort"}]}`,
			Upstream: sseOK(answer("claude-sonnet-5-5-high", "t-limits-2"))},
		{Name: "claude55_gemini_stream", Op: "stream", Source: "gemini", Model: "claude-sonnet-5-5-high",
			Payload:  `{"contents":[{"role":"user","parts":[{"text":"limits"}]}],"generationConfig":{"maxOutputTokens":500000,"thinkingConfig":{"thinkingBudget":-1}}}`,
			Upstream: sseOK(answer("claude-sonnet-5-5-high", "t-limits-3"))},
	}
}
