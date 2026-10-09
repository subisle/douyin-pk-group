package bot

// Command is the closed contract consumed by robot adapters.
type Command string

const (
	CommandHelp  Command = "help"
	CommandQuery Command = "query"
	CommandRank  Command = "rank"
	CommandGroup Command = "group"
)

// Request is deterministic: adapters map platform messages to this shape.
// The command router must not invoke an AI model.
type Request struct {
	Command Command  `json:"command"`
	Args    []string `json:"args"`
}

// Response is the deterministic output sent back to a robot channel.
type Response struct {
	Text string `json:"text"`
}
