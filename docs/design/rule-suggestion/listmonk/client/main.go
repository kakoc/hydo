// lmc: the test client of the listmonk onboarding (Milestone 236's unseen business app). Every
// workload operation goes through listmonk's HTTP API with the admin's basic auth, and is
// recorded as a token: in $HYDO_ACKED when the client saw it acknowledged, and in
// $HYDO_OUTCOMES as "<token> ack|failed|unknown". A token is <op>~<details>:
//
//	n<id>~<email>          a new subscriber on both lists
//	b<id>~<email>,hard     a hard bounce of a base subscriber (bounce webhook)
//	u<id>~<email>,<list>   an unsubscription of a base subscriber from a list
//	k<id>~<email>          a blocklisting of a base subscriber
//
// <id> is the composer's entry number ($HYDO_CMD_ID). Base subscribers are s1..s6@ex.com.
package main

import (
	"bytes"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"os"
	"strconv"
	"strings"
	"time"
)

var base = envOr("LM_URL", "http://10.89.0.12:9000")
var client = &http.Client{Timeout: 20 * time.Second}

func envOr(k, d string) string {
	if v := os.Getenv(k); v != "" {
		return v
	}
	return d
}

func do(method, path string, body any) (int, []byte, error) {
	var rd io.Reader
	if body != nil {
		b, _ := json.Marshal(body)
		rd = bytes.NewReader(b)
	}
	req, err := http.NewRequest(method, base+path, rd)
	if err != nil {
		return 0, nil, err
	}
	req.SetBasicAuth("listmonk", "listmonk")
	if body != nil {
		req.Header.Set("Content-Type", "application/json")
	}
	resp, err := client.Do(req)
	if err != nil {
		return 0, nil, err
	}
	defer resp.Body.Close()
	b, err := io.ReadAll(resp.Body)
	if err != nil {
		return 0, nil, err
	}
	return resp.StatusCode, b, nil
}

func appendLine(path, line string) {
	if path == "" {
		return
	}
	f, err := os.OpenFile(path, os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0o644)
	if err != nil {
		os.Exit(3)
	}
	defer f.Close()
	f.WriteString(line + "\n")
}

type outcome int

const (
	ack outcome = iota
	failed
	unknown
)

func record(token string, o outcome) {
	switch o {
	case ack:
		appendLine(os.Getenv("HYDO_ACKED"), token)
		appendLine(os.Getenv("HYDO_OUTCOMES"), token+" ack")
	case failed:
		appendLine(os.Getenv("HYDO_OUTCOMES"), token+" failed")
	default:
		appendLine(os.Getenv("HYDO_OUTCOMES"), token+" unknown")
	}
}

// send runs one request with up to 3 attempts after no answer; a 2xx is acknowledged, a 4xx
// failed, anything else unknown.
func send(method, path string, body any) outcome {
	for i := 0; i < 3; i++ {
		st, b, err := do(method, path, body)
		if err != nil {
			time.Sleep(time.Second)
			continue
		}
		switch {
		case st >= 200 && st < 300:
			return ack
		case st >= 400 && st < 500:
			fmt.Printf("%s %s: %d %s\n", method, path, st, strings.TrimSpace(string(b)))
			return failed
		default:
			fmt.Printf("%s %s: %d %s\n", method, path, st, strings.TrimSpace(string(b)))
			return unknown
		}
	}
	return unknown
}

func seed() int {
	n, _ := strconv.Atoi(os.Getenv("HYDO_SEED"))
	if n < 0 {
		n = -n
	}
	return n
}

func listID(name string) int {
	st, b, err := do("GET", "/api/lists?per_page=all", nil)
	if err != nil || st != 200 {
		fmt.Printf("lists: %d %v\n", st, err)
		os.Exit(2)
	}
	var r struct {
		Data struct {
			Results []struct {
				ID   int    `json:"id"`
				Name string `json:"name"`
			} `json:"results"`
		} `json:"data"`
	}
	json.Unmarshal(b, &r)
	for _, l := range r.Data.Results {
		if l.Name == name {
			return l.ID
		}
	}
	fmt.Printf("no list %s\n", name)
	os.Exit(2)
	return 0
}

func subID(email string) int {
	q := "subscribers.email='" + email + "'"
	st, b, err := do("GET", "/api/subscribers?per_page=1&query="+urlq(q), nil)
	if err != nil || st != 200 {
		return 0
	}
	var r struct {
		Data struct {
			Results []struct {
				ID int `json:"id"`
			} `json:"results"`
		} `json:"data"`
	}
	json.Unmarshal(b, &r)
	if len(r.Data.Results) == 0 {
		return 0
	}
	return r.Data.Results[0].ID
}

func urlq(s string) string {
	var o strings.Builder
	for _, c := range []byte(s) {
		if c >= 'a' && c <= 'z' || c >= 'A' && c <= 'Z' || c >= '0' && c <= '9' || c == '.' || c == '_' || c == '-' {
			o.WriteByte(c)
		} else {
			fmt.Fprintf(&o, "%%%02X", c)
		}
	}
	return o.String()
}

func main() {
	if len(os.Args) < 2 {
		fmt.Println("usage: lmc ready|init|op <kind>")
		os.Exit(2)
	}
	id := os.Getenv("HYDO_CMD_ID")
	s := seed()
	switch os.Args[1] {
	case "ready":
		st, _, err := do("GET", "/api/health", nil)
		if err != nil || st != 200 {
			os.Exit(1)
		}
	case "init":
		// two lists, bounce processing on (the webhook), and six base subscribers on both lists
		for _, l := range []map[string]any{
			{"name": "news", "type": "public", "optin": "single"},
			{"name": "digest", "type": "public", "optin": "double"},
		} {
			if o := send("POST", "/api/lists", l); o != ack {
				fmt.Println("init: list", l["name"])
				os.Exit(1)
			}
		}
		st, b, err := do("GET", "/api/settings", nil)
		if err != nil || st != 200 {
			fmt.Println("init: settings", st, err)
			os.Exit(1)
		}
		var set struct {
			Data map[string]any `json:"data"`
		}
		json.Unmarshal(b, &set)
		set.Data["bounce.enabled"] = true
		set.Data["bounce.webhooks_enabled"] = true
		if o := send("PUT", "/api/settings", set.Data); o != ack {
			fmt.Println("init: settings update")
			os.Exit(1)
		}
		// the settings reload restarts the app: wait for it
		for i := 0; i < 60; i++ {
			time.Sleep(time.Second)
			if st, _, err := do("GET", "/api/health", nil); err == nil && st == 200 {
				break
			}
		}
		news, digest := listID("news"), listID("digest")
		for i := 1; i <= 6; i++ {
			e := fmt.Sprintf("s%d@ex.com", i)
			o := send("POST", "/api/subscribers", map[string]any{
				"email": e, "name": "Base " + strconv.Itoa(i), "status": "enabled",
				"lists": []int{news, digest}, "preconfirm_subscriptions": true})
			if o != ack {
				fmt.Println("init: subscriber", e)
				os.Exit(1)
			}
		}
		fmt.Println("init: 2 lists, 6 subscribers, bounce webhooks on")
	case "op":
		kind := os.Args[2]
		baseEmail := fmt.Sprintf("s%d@ex.com", 1+s%6)
		var token string
		var o outcome
		switch kind {
		case "subscribe": // a new subscriber on both lists, without confirmation
			e := "n" + id + "@ex.com"
			token = "n" + id + "~" + e
			record(token, unknown)
			o = send("POST", "/api/subscribers", map[string]any{
				"email": e, "name": "New " + id, "status": "enabled",
				"lists": []int{listID("news"), listID("digest")}})
		case "bounce": // a hard bounce of a base subscriber
			token = "b" + id + "~" + baseEmail + ",hard"
			record(token, unknown)
			o = send("POST", "/webhooks/bounce", map[string]any{
				"email": baseEmail, "source": "hydo", "type": "hard"})
		case "unsubscribe": // a base subscriber leaves the news list
			sid := subID(baseEmail)
			if sid == 0 {
				fmt.Println("no subscriber", baseEmail)
				os.Exit(1)
			}
			token = "u" + id + "~" + baseEmail + ",news"
			record(token, unknown)
			o = send("PUT", "/api/subscribers/lists", map[string]any{
				"ids": []int{sid}, "action": "unsubscribe", "target_list_ids": []int{listID("news")}})
		case "blocklist": // the admin blocklists a base subscriber
			sid := subID(baseEmail)
			if sid == 0 {
				fmt.Println("no subscriber", baseEmail)
				os.Exit(1)
			}
			token = "k" + id + "~" + baseEmail
			record(token, unknown)
			o = send("PUT", "/api/subscribers/"+strconv.Itoa(sid)+"/blocklist", nil)
		default:
			fmt.Println("unknown op", kind)
			os.Exit(2)
		}
		record(token, o)
		if o != ack {
			os.Exit(1)
		}
	default:
		os.Exit(2)
	}
}
