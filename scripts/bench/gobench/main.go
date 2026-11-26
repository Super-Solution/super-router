package main

import (
	"context"
	"database/sql"
	_ "github.com/jackc/pgx/v5/stdlib"
	"github.com/Super-Solution/super-router/backends/go/store/postgres"
	"log"
	"net"
	"net/http"
	"os"

	"github.com/Super-Solution/super-router/backends/go/contracts"
	"github.com/Super-Solution/super-router/backends/go/provider/openai"
	"github.com/Super-Solution/super-router/backends/go/router"
	"github.com/Super-Solution/super-router/backends/go/server"
	"github.com/Super-Solution/super-router/backends/go/store/memory"
)

func main() {
	up := os.Getenv("UPSTREAM")
	if up == "" {
		up = "http://127.0.0.1:3499"
	}
	var store router.DataStore = memory.New(nil)
	if url := os.Getenv("DATABASE_URL"); url != "" {
		db, err := sql.Open("pgx", url)
		if err != nil {
			log.Fatal(err)
		}
		db.SetMaxOpenConns(20)
		pg := postgres.New(db, postgres.Options{})
		if err := pg.Migrate(context.Background()); err != nil {
			log.Fatal(err)
		}
		store = pg
	}
	providers := map[string]contracts.Provider{
		"openai": openai.New(func() string { return "sk-fake" }, openai.WithBaseURL(up)),
		"mock":   router.NewMockProvider("success"),
	}
	rt, err := router.New(store, providers, "bench-digest-key-0123456789abcdef-0123")
	if err != nil {
		log.Fatal(err)
	}
	owner := contracts.Principal{Workspace: "ws", Environment: "prod", Customer: "c1",
		Scopes: []string{"invoke", "service:write", "billing:write", "billing:read", "operations:read", "routing:write"}}
	auth := server.StaticKeys([]server.Key{{Secret: "sr_" + string(make([]byte, 0)) + "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", Principal: owner}})
	api, err := server.Handler(server.Options{Router: rt, Store: store, Auth: auth})
	if err != nil {
		log.Fatal(err)
	}
	l, err := net.Listen("tcp", "127.0.0.1:3411")
	if err != nil {
		log.Fatal(err)
	}
	log.Fatal(http.Serve(l, api))
}
