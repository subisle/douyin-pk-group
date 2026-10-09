package render

import "context"

// Template is render input and PNG is render output. Implementations must not
// rely on a resident Chromium process.
type Renderer interface {
	RenderPNG(ctx context.Context, template map[string]any) ([]byte, error)
}
