package main

import (
	"bytes"
	"context"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/subisle/douyin-pk-group/server-go/internal/bot"
)

type fakeStore struct{}

func (fakeStore) Query(context.Context, []string) (string, error) {
	return "查询结果", nil
}

func (fakeStore) Rank(context.Context, []string) (string, error) {
	return "排行结果", nil
}

func TestRobotEndpointRejectsUnknownCommand(t *testing.T) {
	setRobotRouter(bot.NewRouter(fakeStore{}))
	body := bytes.NewBufferString(`{"command":"drop","args":[]}`)
	req := httptest.NewRequest(http.MethodPost, "/dy/api/robot/commands", body)
	rec := httptest.NewRecorder()

	robot(rec, req)

	if rec.Code != http.StatusBadRequest {
		t.Fatalf("status = %d", rec.Code)
	}
	if !strings.Contains(rec.Body.String(), "unsupported command") {
		t.Fatalf("body = %s", rec.Body.String())
	}
}

func TestRobotEndpointExecutesHelp(t *testing.T) {
	setRobotRouter(bot.NewRouter(fakeStore{}))
	body := bytes.NewBufferString(`{"command":"help","args":[]}`)
	req := httptest.NewRequest(http.MethodPost, "/dy/api/robot/commands", body)
	rec := httptest.NewRecorder()

	robot(rec, req)

	if rec.Code != http.StatusOK {
		t.Fatalf("status = %d", rec.Code)
	}
	if !strings.Contains(rec.Body.String(), "可用命令") {
		t.Fatalf("body = %s", rec.Body.String())
	}
}
