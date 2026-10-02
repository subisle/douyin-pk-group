package main

import (
	"context"
	"encoding/json"
	"errors"
	"log"
	"net/http"
	"os"
	"time"

	"github.com/go-sql-driver/mysql"
	"github.com/subisle/douyin-pk-group/server-go/internal/bot"
	"github.com/subisle/douyin-pk-group/server-go/internal/data"
)

type botRequest struct {
	Command string   `json:"command"`
	Args    []string `json:"args"`
}

type botResponse struct {
	Text string `json:"text"`
}

var robotRouter *bot.Router
var healthStore interface {
	Health(context.Context) error
}

func setRobotRouter(router *bot.Router) {
	robotRouter = router
	if store, ok := router.Store().(interface{ Health(context.Context) error }); ok {
		healthStore = store
	} else {
		healthStore = nil
	}
}

func loadRobotRouter() (*bot.Router, func(), error) {
	dsn := os.Getenv("DOUYIN_DATABASE_DSN")
	if dsn == "" {
		router := bot.NewRouter(data.NewStaticStore())
		return router, func() {}, nil
	}

	cfg, err := mysql.ParseDSN(dsn)
	if err != nil {
		return nil, nil, err
	}
	cfg.ParseTime = true
	cfg.MultiStatements = true
	store, err := data.Open("mysql", cfg.FormatDSN())
	if err != nil {
		return nil, nil, err
	}
	return bot.NewRouter(store), func() { _ = store.Close() }, nil
}

func applyMigrations(ctx context.Context, store *data.Store, enabled bool) ([]string, error) {
	if !enabled {
		return nil, nil
	}
	return store.MigrateAll(ctx)
}

func health(w http.ResponseWriter, r *http.Request) {
	if healthStore != nil {
		if err := healthStore.Health(r.Context()); err != nil {
			writeJSON(w, http.StatusServiceUnavailable, map[string]any{"ok": false, "server": "douyin-server", "db": err.Error()})
			return
		}
	}
	writeJSON(w, http.StatusOK, map[string]any{"ok": true, "server": "douyin-server"})
}

func robot(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodPost {
		writeJSON(w, http.StatusMethodNotAllowed, map[string]string{"error": "method not allowed"})
		return
	}
	var req botRequest
	if err := json.NewDecoder(http.MaxBytesReader(w, r.Body, 1<<16)).Decode(&req); err != nil {
		writeJSON(w, http.StatusBadRequest, map[string]string{"error": "invalid json"})
		return
	}
	response, err := robotRouter.Dispatch(r.Context(), bot.Request{
		Command: bot.Command(req.Command),
		Args:    req.Args,
	})
	if err != nil {
		writeJSON(w, http.StatusBadRequest, map[string]string{"error": err.Error()})
		return
	}
	writeJSON(w, http.StatusOK, botResponse{Text: response.Text})
}

func writeJSON(w http.ResponseWriter, status int, value any) {
	w.Header().Set("Content-Type", "application/json; charset=utf-8")
	w.WriteHeader(status)
	_ = json.NewEncoder(w).Encode(value)
}

func main() {
	addr := os.Getenv("DOUYIN_HTTP_ADDR")
	if addr == "" {
		addr = ":8080"
	}

	robotRouter, closeStore, err := loadRobotRouter()
	if err != nil {
		log.Fatal(err)
	}
	defer closeStore()
	setRobotRouter(robotRouter)
	if store, ok := robotRouter.Store().(*data.Store); ok {
		applied, err := applyMigrations(context.Background(), store, os.Getenv("DOUYIN_MIGRATE") == "1")
		if err != nil {
			closeStore()
			log.Fatal(err)
		}
		if len(applied) > 0 {
			log.Printf("migrations applied: %v", applied)
		}
	}

	mux := http.NewServeMux()
	mux.HandleFunc("/dy/api/health", health)
	mux.HandleFunc("/dy/api/robot/commands", robot)

	server := &http.Server{
		Addr:              addr,
		Handler:           mux,
		ReadHeaderTimeout: 5 * time.Second,
		ReadTimeout:       15 * time.Second,
		WriteTimeout:      15 * time.Second,
	}
	log.Printf("douyin-server listening on %s", addr)
	if err := server.ListenAndServe(); err != nil && !errors.Is(err, http.ErrServerClosed) {
		log.Fatal(err)
	}
}
