package bot

import (
	"context"
	"fmt"
)

type QueryStore interface {
	Query(ctx context.Context, args []string) (string, error)
	Rank(ctx context.Context, args []string) (string, error)
}

type Router struct {
	store QueryStore
}

func (r *Router) Store() QueryStore {
	return r.store
}

func NewRouter(store QueryStore) *Router {
	return &Router{store: store}
}

func (r *Router) Dispatch(ctx context.Context, request Request) (Response, error) {
	switch request.Command {
	case CommandHelp:
		return Response{Text: HelpText()}, nil
	case CommandQuery:
		return r.text(ctx, r.store.Query, request.Args)
	case CommandRank:
		return r.text(ctx, r.store.Rank, request.Args)
	case CommandGroup:
		return Response{Text: "分组请通过 PK 管理后台生成后使用公布链接。"}, nil
	default:
		return Response{}, fmt.Errorf("unsupported command %q", request.Command)
	}
}

func (r *Router) text(ctx context.Context, fn func(context.Context, []string) (string, error), args []string) (Response, error) {
	text, err := fn(ctx, args)
	if err != nil {
		return Response{}, err
	}
	return Response{Text: text}, nil
}

func HelpText() string {
	return `可用命令：
  查询 <主播名>  查询主播基础数据
  排行 [日期]    查询音浪排行
  分组          获取当前公布链接`
}
