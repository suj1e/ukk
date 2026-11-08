package agents

import (
	"bufio"
	"encoding/json"
	"fmt"
	"os"
	"strings"
	"testing"
	"time"
)

// TestHelperProcess is a fake `omp --mode rpc` child. It speaks the documented
// v1 framing: one JSON object per line on stdout, commands on stdin.
const (
	helperEnv     = "UKK_AGENTS_HELPER"
	scenarioEnv   = "UKK_AGENTS_SCENARIO"
	helperPIDFile = "UKK_AGENTS_PIDFILE"

	// fakeCredential is deliberately token-shaped so the redaction test proves
	// provider credentials never reach an error or diagnostic. It is not a
	// credential.
	fakeCredential = "sk-" + "0123456789abcdef0123456789abcdef"
)

func TestHelperProcess(t *testing.T) {
	if os.Getenv(helperEnv) != "1" {
		return
	}
	if pidFile := os.Getenv(helperPIDFile); pidFile != "" {
		if file, err := os.OpenFile(pidFile, os.O_APPEND|os.O_CREATE|os.O_WRONLY, 0o600); err == nil {
			_, _ = fmt.Fprintf(file, "%d\n", os.Getpid())
			_ = file.Close()
		}
	}
	runHelper(os.Getenv(scenarioEnv))
	os.Exit(0)
}

type helperCommand struct {
	Type      string `json:"type"`
	ID        string `json:"id"`
	Message   string `json:"message"`
	Value     string `json:"value"`
	Confirmed *bool  `json:"confirmed"`
	Cancelled *bool  `json:"cancelled"`
}

func runHelper(scenario string) {
	writer := bufio.NewWriterSize(os.Stdout, 64<<10)
	emit := func(value any) {
		data, err := json.Marshal(value)
		if err != nil {
			os.Exit(9)
		}
		_, _ = writer.Write(data)
		_ = writer.WriteByte('\n')
		_ = writer.Flush()
	}
	respond := func(id, command string, data any) {
		frame := map[string]any{"type": "response", "id": id, "command": command, "success": true}
		if data != nil {
			frame["data"] = data
		}
		emit(frame)
	}
	reject := func(id, command, message, code string) {
		emit(map[string]any{
			"type": "response", "id": id, "command": command,
			"success": false, "error": message, "code": code,
		})
	}

	if scenario == "noready" {
		for {
			time.Sleep(time.Hour)
		}
	}

	emit(map[string]any{
		"type": "ready", "protocolVersion": 1,
		"supportedProtocolVersions": []int{1, 2},
		"maxFrameBytes":             1 << 20,
		"maxReassembledFrameBytes":  1 << 26,
	})
	emit(map[string]any{"type": "available_commands_update", "commands": []any{}})

	if scenario == "stderr-secret" {
		fmt.Fprintln(os.Stderr, "provider auth failed for "+fakeCredential)
		time.Sleep(20 * time.Millisecond)
	}

	running := false
	approvalPending := false
	reader := bufio.NewReader(os.Stdin)
	for {
		line, err := reader.ReadString('\n')
		if err != nil {
			return
		}
		var command helperCommand
		if err := json.Unmarshal([]byte(line), &command); err != nil {
			continue
		}
		switch command.Type {
		case "get_state":
			respond(command.ID, "get_state", map[string]any{
				"sessionId": "mock-session-1", "isStreaming": false, "messageCount": 0,
			})
		case "prompt":
			if running && scenario == "hold" {
				// A correct runtime never reaches this: the second prompt is
				// rejected before it is written. Emitting a marker makes a
				// double admission visible to the test.
				emit(map[string]any{"type": "notice", "level": "warning", "message": "DOUBLE-PROMPT"})
				respond(command.ID, "prompt", map[string]any{"agentInvoked": true})
				continue
			}
			switch scenario {
			case "text":
				respond(command.ID, "prompt", map[string]any{"agentInvoked": true})
				emit(map[string]any{"type": "agent_start"})
				emit(map[string]any{"type": "turn_start"})
				emit(map[string]any{"type": "message_start", "messageId": "msg-1", "message": map[string]any{"role": "assistant", "content": []any{}}})
				emit(map[string]any{"type": "message_update", "messageId": "msg-1", "message": map[string]any{"role": "assistant", "content": []any{}}, "assistantMessageEvent": map[string]any{"type": "text_delta", "contentIndex": 0, "delta": "OMP-"}})
				emit(map[string]any{"type": "message_update", "messageId": "msg-1", "message": map[string]any{"role": "assistant", "content": []any{}}, "assistantMessageEvent": map[string]any{"type": "text_delta", "contentIndex": 0, "delta": "MOCK"}})
				emit(map[string]any{"type": "message_update", "messageId": "msg-1", "message": map[string]any{"role": "assistant", "content": []any{}}, "assistantMessageEvent": map[string]any{"type": "done", "reason": "stop", "message": map[string]any{"role": "assistant", "content": []any{map[string]any{"type": "text", "text": "OMP-MOCK"}}}}})
				emit(map[string]any{"type": "message_end", "messageId": "msg-1", "message": map[string]any{"role": "assistant", "content": []any{map[string]any{"type": "text", "text": "OMP-MOCK"}}, "stopReason": "stop", "usage": map[string]any{"input": 10, "output": 5, "cacheRead": 2, "cacheWrite": 0, "totalTokens": 17, "cost": map[string]any{"total": 0.001}}}})
				emit(map[string]any{"type": "turn_end", "message": map[string]any{"role": "assistant", "content": []any{}}})
				emit(map[string]any{"type": "agent_end", "messages": []any{}, "isTerminal": true})
			case "tool":
				respond(command.ID, "prompt", map[string]any{"agentInvoked": true})
				emit(map[string]any{"type": "agent_start"})
				emit(map[string]any{"type": "turn_start"})
				emit(map[string]any{"type": "message_start", "messageId": "msg-tool", "message": map[string]any{"role": "assistant", "content": []any{}}})
				emit(map[string]any{"type": "tool_execution_start", "toolCallId": "tool-1", "toolName": "bash", "args": map[string]any{"command": "echo hi"}})
				emit(map[string]any{"type": "tool_execution_update", "toolCallId": "tool-1", "toolName": "bash", "partialResult": map[string]any{"content": []any{map[string]any{"type": "text", "text": "hi\n"}}}})
				emit(map[string]any{"type": "tool_execution_end", "toolCallId": "tool-1", "toolName": "bash", "result": map[string]any{"content": []any{map[string]any{"type": "text", "text": "hi\n"}}}, "isError": false})
				emit(map[string]any{"type": "message_update", "messageId": "msg-tool", "message": map[string]any{"role": "assistant", "content": []any{}}, "assistantMessageEvent": map[string]any{"type": "text_delta", "contentIndex": 0, "delta": "TOOL-OK"}})
				emit(map[string]any{"type": "message_end", "messageId": "msg-tool", "message": map[string]any{"role": "assistant", "content": []any{map[string]any{"type": "text", "text": "TOOL-OK"}}}})
				emit(map[string]any{"type": "agent_end", "messages": []any{}, "isTerminal": true})
			case "approval":
				respond(command.ID, "prompt", map[string]any{"agentInvoked": true})
				emit(map[string]any{"type": "agent_start"})
				emit(map[string]any{"type": "turn_start"})
				emit(map[string]any{"type": "message_start", "messageId": "msg-approval", "message": map[string]any{"role": "assistant", "content": []any{}}})
				emit(map[string]any{"type": "tool_execution_start", "toolCallId": "tool-approval", "toolName": "bash", "args": map[string]any{"command": "echo approved"}})
				emit(map[string]any{
					"type": "extension_ui_request", "id": "approval-1", "method": "select",
					"title": "Allow tool: bash", "options": []string{"Approve", "Deny"},
				})
				approvalPending = true
			case "hold":
				respond(command.ID, "prompt", map[string]any{"agentInvoked": true})
				emit(map[string]any{"type": "agent_start"})
				emit(map[string]any{"type": "turn_start"})
				emit(map[string]any{"type": "message_start", "messageId": "msg-hold", "message": map[string]any{"role": "assistant", "content": []any{}}})
				emit(map[string]any{"type": "message_update", "messageId": "msg-hold", "message": map[string]any{"role": "assistant", "content": []any{}}, "assistantMessageEvent": map[string]any{"type": "text_delta", "contentIndex": 0, "delta": "HOLD"}})
				running = true
			case "exit":
				os.Exit(3)
			case "exit-secret", "stderr-secret":
				os.Exit(2)
			case "malformed":
				respond(command.ID, "prompt", map[string]any{"agentInvoked": true})
				_, _ = writer.WriteString("{\"type\":\n")
				_ = writer.Flush()
				for {
					time.Sleep(time.Hour)
				}
			case "stderr-flood":
				respond(command.ID, "prompt", map[string]any{"agentInvoked": true})
				flood := strings.Repeat("x", 64<<10)
				for range 6 {
					_, _ = os.Stderr.WriteString(flood)
				}
				for {
					time.Sleep(time.Hour)
				}
			case "reject":
				reject(command.ID, "prompt", "Model not found: provider/model", "model_not_found")
			case "local":
				respond(command.ID, "prompt", map[string]any{"agentInvoked": false})
				emit(map[string]any{"type": "command_output", "text": "local handled"})
				emit(map[string]any{"type": "prompt_result", "id": command.ID, "agentInvoked": false, "status": "completed"})
			case "burst":
				respond(command.ID, "prompt", map[string]any{"agentInvoked": true})
				emit(map[string]any{"type": "agent_start"})
				emit(map[string]any{"type": "turn_start"})
				emit(map[string]any{"type": "message_start", "messageId": "msg-burst", "message": map[string]any{"role": "assistant", "content": []any{}}})
				for range 600 {
					emit(map[string]any{"type": "message_update", "messageId": "msg-burst", "message": map[string]any{"role": "assistant", "content": []any{}}, "assistantMessageEvent": map[string]any{"type": "text_delta", "contentIndex": 0, "delta": "x"}})
				}
				emit(map[string]any{"type": "message_end", "messageId": "msg-burst", "message": map[string]any{"role": "assistant", "content": []any{map[string]any{"type": "text", "text": strings.Repeat("x", 600)}}}})
				emit(map[string]any{"type": "agent_end", "messages": []any{}, "isTerminal": true})
			default:
				respond(command.ID, "prompt", map[string]any{"agentInvoked": true})
			}
			if scenario == "hold" {
				running = true
			}
		case "steer", "follow_up":
			respond(command.ID, command.Type, map[string]any{"received": command.Type})
		case "abort":
			respond(command.ID, "abort", nil)
			if scenario == "hold" || running {
				emit(map[string]any{"type": "prompt_result", "id": command.ID, "agentInvoked": true, "status": "aborted"})
				emit(map[string]any{"type": "agent_end", "messages": []any{}, "isTerminal": true})
				running = false
			}
		case "extension_ui_response":
			if !approvalPending {
				continue
			}
			approvalPending = false
			approved := command.Value == "Approve" || (command.Confirmed != nil && *command.Confirmed)
			emit(map[string]any{"type": "tool_execution_end", "toolCallId": "tool-approval", "toolName": "bash", "result": map[string]any{"content": []any{map[string]any{"type": "text", "text": "approved"}}}, "isError": !approved})
			text := "APPROVED"
			if !approved {
				text = "DENIED"
			}
			emit(map[string]any{"type": "message_end", "messageId": "msg-approval", "message": map[string]any{"role": "assistant", "content": []any{map[string]any{"type": "text", "text": text}}}})
			emit(map[string]any{"type": "agent_end", "messages": []any{}, "isTerminal": true})
		default:
			respond(command.ID, command.Type, nil)
		}
	}
}
