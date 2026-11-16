package main

// The WebSocket RPC endpoint /rpc, for live queries: JSON requests {id, method, params}, answers
// {id, result | error}, and notifications {result: {action, id, result}} without an id.

import (
	"encoding/json"
	"os"
	"errors"
	"fmt"
	"net"
	"strings"
	"time"

	"golang.org/x/net/websocket"
)

var errTimeout = errors.New("timeout")

type wsConn struct {
	c       *websocket.Conn
	n       int
	pending [][]byte // notifications read while waiting for an answer
}

func wsDial() (*wsConn, error) {
	url := strings.Replace(base, "http", "ws", 1) + "/rpc"
	cfg, err := websocket.NewConfig(url, base)
	if err != nil {
		return nil, err
	}
	cfg.Protocol = []string{"json"}
	cfg.Dialer = &net.Dialer{Timeout: 5 * time.Second}
	c, err := websocket.DialConfig(cfg)
	if err != nil {
		return nil, err
	}
	w := &wsConn{c: c}
	if _, err := w.call("signin", []any{map[string]string{"user": "root", "pass": "root"}}); err != nil {
		c.Close()
		return nil, fmt.Errorf("signin: %w", err)
	}
	if _, err := w.call("use", []any{"hydo", "hydo"}); err != nil {
		c.Close()
		return nil, fmt.Errorf("use: %w", err)
	}
	return w, nil
}

func (w *wsConn) Close() { w.c.Close() }

// call sends one request and returns its result, keeping notifications that arrive meanwhile.
func (w *wsConn) call(method string, params []any) (json.RawMessage, error) {
	w.n++
	id := w.n
	req, _ := json.Marshal(map[string]any{"id": id, "method": method, "params": params})
	w.c.SetWriteDeadline(time.Now().Add(10 * time.Second))
	if err := websocket.Message.Send(w.c, string(req)); err != nil {
		return nil, err
	}
	deadline := time.Now().Add(10 * time.Second)
	for {
		msg, err := w.read(time.Until(deadline))
		if err != nil {
			return nil, err
		}
		var ans struct {
			ID     *int            `json:"id"`
			Result json.RawMessage `json:"result"`
			Error  json.RawMessage `json:"error"`
		}
		if json.Unmarshal(msg, &ans) != nil {
			continue
		}
		if ans.ID == nil {
			w.pending = append(w.pending, msg)
			continue
		}
		if *ans.ID != id {
			continue
		}
		if len(ans.Error) > 0 && string(ans.Error) != "null" {
			return nil, fmt.Errorf("%s: %s", method, trunc(string(ans.Error), 200))
		}
		return ans.Result, nil
	}
}

// next returns the next notification, or errTimeout.
func (w *wsConn) next(wait time.Duration) ([]byte, error) {
	if len(w.pending) > 0 {
		m := w.pending[0]
		w.pending = w.pending[1:]
		return m, nil
	}
	return w.read(wait)
}

func (w *wsConn) read(wait time.Duration) ([]byte, error) {
	if wait <= 0 {
		return nil, errTimeout
	}
	w.c.SetReadDeadline(time.Now().Add(wait))
	var s string
	err := websocket.Message.Receive(w.c, &s)
	if os.Getenv("SDB_DEBUG") != "" {
		fmt.Fprintf(os.Stderr, "ws< %q %v\n", trunc(s, 300), err)
	}
	if err != nil {
		var ne net.Error
		if errors.As(err, &ne) && ne.Timeout() {
			return nil, errTimeout
		}
		return nil, err
	}
	return []byte(s), nil
}
