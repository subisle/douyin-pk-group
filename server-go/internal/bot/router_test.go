package bot

import (
	"context"
	"testing"
)

type fakeStore struct {
	query func(context.Context, []string) (string, error)
	rank  func(context.Context, []string) (string, error)
}

func (s fakeStore) Query(ctx context.Context, args []string) (string, error) {
	return s.query(ctx, args)
}

func (s fakeStore) Rank(ctx context.Context, args []string) (string, error) {
	return s.rank(ctx, args)
}

func TestRouterIsDeterministic(t *testing.T) {
	router := NewRouter(fakeStore{
		query: func(context.Context, []string) (string, error) { return "查询结果", nil },
		rank:  func(context.Context, []string) (string, error) { return "排行结果", nil },
	})

	got, err := router.Dispatch(context.Background(), Request{Command: CommandQuery, Args: []string{"阿楠"}})
	if err != nil {
		t.Fatal(err)
	}
	if got.Text != "查询结果" {
		t.Fatalf("got %q", got.Text)
	}
}

func TestRouterRejectsUnknownCommand(t *testing.T) {
	router := NewRouter(fakeStore{})
	if _, err := router.Dispatch(context.Background(), Request{Command: Command("drop")}); err == nil {
		t.Fatal("expected unknown command error")
	}
}
