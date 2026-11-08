package bridge

import (
	"context"
	"os"
	"path/filepath"

	"github.com/suj1e/ukk/apps/desktop/internal/extensions"
)

// PluginService exposes the plug-in supervisor through the bridge. Only an
// explicit local action may install or enable code.
type PluginService struct {
	supervisor *extensions.Supervisor
}

// NewPluginService opens durable grants and the plugin root.
func NewPluginService(dir string) (*PluginService, error) {
	root := filepath.Join(dir, "plugins")
	if err := os.MkdirAll(root, 0o700); err != nil {
		return nil, err
	}
	grants, err := extensions.OpenGrantStore(filepath.Join(dir, "plugin-grants.json"))
	if err != nil {
		return nil, err
	}
	return &PluginService{supervisor: extensions.NewSupervisor(extensions.Config{
		Root:   root,
		Grants: grants,
		Audit:  extensions.NewAudit(512),
	})}, nil
}

// List returns installed plugins.
func (p *PluginService) List(context.Context) []extensions.PluginStatus {
	return p.supervisor.List()
}

// Install validates a plugin directory and registers it.
func (p *PluginService) Install(_ context.Context, params PluginInstallParams) (extensions.PluginStatus, error) {
	manifest, err := p.supervisor.InstallDir(params.Dir, extensions.InstallOptions{Dir: params.Dir})
	if err != nil {
		return extensions.PluginStatus{}, &Error{Code: "plugin_install_failed", Message: err.Error()}
	}
	return extensions.PluginStatus{
		ID: manifest.ID, Name: manifest.Name, Version: manifest.Version,
		Trust: string(manifest.Trust), State: extensions.StateDisabled,
		Commands: len(manifest.Commands()), Views: len(manifest.Views()), Events: len(manifest.Events()),
	}, nil
}

// Enable starts an installed plugin.
func (p *PluginService) Enable(ctx context.Context, id string) error {
	if err := p.supervisor.Enable(ctx, id); err != nil {
		return &Error{Code: "plugin_enable_failed", Message: err.Error()}
	}
	return nil
}

// Disable stops a plugin and kills its helper.
func (p *PluginService) Disable(id string) error {
	if err := p.supervisor.Disable(id); err != nil {
		return &Error{Code: "plugin_disable_failed", Message: err.Error()}
	}
	return nil
}

// Revoke removes one grant.
func (p *PluginService) Revoke(plugin, capability, scope string) error {
	if _, err := p.supervisor.Revoke(plugin, capability, scope); err != nil {
		return &Error{Code: "plugin_revoke_failed", Message: err.Error()}
	}
	return nil
}

// Grant adds one capability grant.
func (p *PluginService) Grant(plugin, capability, scope string) error {
	if _, err := p.supervisor.Grants().Grant(plugin, capability, scope, 0); err != nil {
		return &Error{Code: "plugin_grant_failed", Message: err.Error()}
	}
	return nil
}

// Invoke runs one declared command.
func (p *PluginService) Invoke(ctx context.Context, params PluginInvokeParams) (extensions.InvokeResult, error) {
	result, err := p.supervisor.Invoke(ctx, extensions.InvokeRequest{
		Plugin: params.Plugin, Command: params.Command,
		SessionID: params.SessionID, Args: params.Args,
	})
	if err != nil {
		return extensions.InvokeResult{}, &Error{Code: "plugin_invoke_failed", Message: err.Error()}
	}
	return result, nil
}

// Shutdown stops every plugin.
func (p *PluginService) Shutdown() { p.supervisor.Shutdown() }
