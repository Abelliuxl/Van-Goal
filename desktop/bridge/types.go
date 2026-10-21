package bridge

// Update is what one poll returns: only the parts that changed are set.
type Update struct {
	State    *State          `json:"state"`
	Messages *[]Message      `json:"messages"`
	Revision uint64          `json:"revision"`
	Deltas   []Delta         `json:"deltas"`
	Settings *Settings       `json:"settings"`
	Composer []ComposerEvent `json:"composer"`
	Error    string          `json:"error"`
}

type Delta struct {
	ID      string `json:"id"`
	Content string `json:"content"`
}

// ComposerEvent tells the composer to clear its draft, or to take one back.
type ComposerEvent struct {
	Action      string   `json:"action"`
	Text        string   `json:"text"`
	Attachments []string `json:"attachments"`
}

type Message struct {
	ID          string       `json:"id"`
	Role        string       `json:"role"`
	Content     string       `json:"content"`
	Streaming   bool         `json:"streaming"`
	Timestamp   float64      `json:"timestamp"`
	CompletedAt *float64     `json:"completed_at"`
	Tools       []Tool       `json:"tools"`
	Attachments []Attachment `json:"attachments"`
}

type Tool struct {
	ID        string  `json:"id"`
	Name      string  `json:"name"`
	Status    string  `json:"status"`
	Detail    string  `json:"detail"`
	Truncated bool    `json:"truncated"`
	Timestamp float64 `json:"timestamp"`
}

type Attachment struct {
	Path   string `json:"path"`
	Name   string `json:"name"`
	Remote bool   `json:"remote"`
}

type State struct {
	Backend      Backend    `json:"backend"`
	Connection   Connection `json:"connection"`
	Reconnecting bool       `json:"reconnecting"`
	Sessions     []Session  `json:"sessions"`
	Selected     *Session   `json:"selected"`
	Sending      bool       `json:"sending"`
	Stopping     bool       `json:"stopping"`
	Refreshing   bool       `json:"refreshing"`
	Queue        []Queued   `json:"queue"`
	Clarify      *Clarify   `json:"clarify"`
	Error        *string    `json:"error"`
	Context      Context    `json:"context"`
	Model        Model      `json:"model"`
	Permission   Permission `json:"permission"`
	CacheSummary string     `json:"cache_summary"`
	Revision     uint64     `json:"revision"`
}

type Backend struct {
	ID      string `json:"id"`
	Name    string `json:"name"`
	Enabled bool   `json:"enabled"`
	Caps    Caps   `json:"caps"`
}

type Caps struct {
	History     bool `json:"history"`
	Resume      bool `json:"resume"`
	Interrupt   bool `json:"interrupt"`
	Interaction bool `json:"interaction"`
	Tools       bool `json:"tools"`
	Attachments bool `json:"attachments"`
	Models      bool `json:"models"`
	Permissions bool `json:"permissions"`
}

// Connection.State is one of disconnected, connecting, connected, degraded,
// failed.
type Connection struct {
	State string `json:"state"`
	Label string `json:"label"`
	Pill  string `json:"pill"`
}

type Session struct {
	ID         string   `json:"id"`
	Title      string   `json:"title"`
	Subtitle   string   `json:"subtitle"`
	Active     bool     `json:"active"`
	Messages   *int64   `json:"messages"`
	Profile    *string  `json:"profile"`
	LastActive *float64 `json:"last_active"`
}

type Queued struct {
	ID          string   `json:"id"`
	Text        string   `json:"text"`
	Attachments []string `json:"attachments"`
}

type Clarify struct {
	Question string   `json:"question"`
	Choices  []string `json:"choices"`
}

type Context struct {
	Used     int64   `json:"used"`
	Max      int64   `json:"max"`
	Measured bool    `json:"measured"`
	Ratio    float64 `json:"ratio"`
}

type Model struct {
	Name         *string      `json:"name"`
	SessionModel *string      `json:"session_model"`
	Provider     string       `json:"provider"`
	Switching    bool         `json:"switching"`
	Groups       []ModelGroup `json:"groups"`
}

type ModelGroup struct {
	Provider string   `json:"provider"`
	Models   []string `json:"models"`
}

type Permission struct {
	Mode     string `json:"mode"`
	Label    string `json:"label"`
	Changing bool   `json:"changing"`
}

type Settings struct {
	Backends         []BackendOption `json:"backends"`
	Backend          string          `json:"backend"`
	Appearance       string          `json:"appearance"`
	Appearances      []Choice        `json:"appearances"`
	FontSize         string          `json:"font_size"`
	FontScale        float32         `json:"font_scale"`
	UIScale          float32         `json:"ui_scale"`
	FontSizes        []Choice        `json:"font_sizes"`
	Host             string          `json:"host"`
	Port             int             `json:"port"`
	UseTLS           bool            `json:"use_tls"`
	URL              string          `json:"url"`
	Credential       string          `json:"credential"`
	CredentialChars  int             `json:"credential_chars"`
	DeviceTokenChars int             `json:"device_token_chars"`
	Workspace        string          `json:"workspace"`
	Profile          string          `json:"profile"`
	ManagedLabel     *string         `json:"managed_label"`
	ManagedWorkspace bool            `json:"managed_workspace"`
	DebugLogging     bool            `json:"debug_logging"`
	LogPath          string          `json:"log_path"`
	SidebarOpen      bool            `json:"sidebar_open"`
	ShowToolCalls    bool            `json:"show_tool_calls"`
	Window           *Window         `json:"window"`
}

type BackendOption struct {
	ID              string `json:"id"`
	Name            string `json:"name"`
	Description     string `json:"description"`
	Enabled         bool   `json:"enabled"`
	Network         bool   `json:"network"`
	CredentialLabel string `json:"credential_label"`
}

type Choice struct {
	ID   string `json:"id"`
	Name string `json:"name"`
}

type Window struct {
	X         float64 `json:"x"`
	Y         float64 `json:"y"`
	Width     float64 `json:"width"`
	Height    float64 `json:"height"`
	Maximized bool    `json:"maximized"`
}
