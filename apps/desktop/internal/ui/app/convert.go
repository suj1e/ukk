package app

import (
	"github.com/suj1e/ukk/apps/desktop/internal/agents"
	"github.com/suj1e/ukk/apps/desktop/internal/bridge"
)

// toAgentSnapshot converts the bridge's wire snapshot into the view model.
func toAgentSnapshot(in bridge.AgentSnapshot) agents.Snapshot {
	out := agents.Snapshot{
		Provider:          in.Provider,
		ProviderSessionID: in.ProviderSessionID,
		State:             agents.State(in.State),
		Provenance:        in.Provenance,
		UpdatedMS:         in.UpdatedMS,
	}
	for _, turn := range in.Turns {
		converted := agents.Turn{
			ID: turn.ID, State: agents.State(turn.State),
			StartedMS: turn.StartedMS, EndedMS: turn.EndedMS,
		}
		if turn.Error != "" {
			converted.Error = &agents.Error{Code: agents.CodeTurnFailed, Message: turn.Error}
		}
		out.Turns = append(out.Turns, converted)
	}
	for _, message := range in.Messages {
		out.Messages = append(out.Messages, agents.Message{
			ID: message.ID, Role: agents.Role(message.Role), Text: message.Text,
			Final: message.Final, CreatedMS: message.CreatedMS,
		})
	}
	for _, tool := range in.Tools {
		out.Tools = append(out.Tools, agents.ToolCall{
			ID: tool.ID, TurnID: tool.TurnID, Name: tool.Name,
			Input: tool.Input, Output: tool.Output,
			Status: agents.ToolStatus(tool.Status), IsError: tool.IsError,
			StartedMS: tool.StartedMS, EndedMS: tool.EndedMS,
		})
	}
	for _, approval := range in.PendingApprovals {
		out.PendingApprovals = append(out.PendingApprovals, agents.Approval{
			ID: approval.ID, TurnID: approval.TurnID, ToolName: approval.ToolName,
			Input: approval.Input, State: agents.ApprovalState(approval.State),
			ExpiresMS: approval.ExpiresMS,
		})
	}
	if in.Usage.TotalTokens > 0 {
		out.Usage.Reported = true
		out.Usage.TotalTokens = in.Usage.TotalTokens
		out.Usage.InputTokens = in.Usage.PromptTokens
		out.Usage.OutputTokens = in.Usage.CompletionTokens
	}
	if in.Error != "" {
		out.Error = &agents.Error{Code: agents.CodeTurnFailed, Message: in.Error}
	}
	return out
}
