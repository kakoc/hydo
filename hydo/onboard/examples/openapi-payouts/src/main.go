// market: a small marketplace service for hydo's example of a fake made from an OpenAPI
// description (hydo.yaml next to src/).
//
//	market server      take orders on 127.0.0.1:8080: POST /orders/<n> pays the seller of order n
//	                   through the payouts provider (its API is ../api/payouts.yaml; in the guest
//	                   the fake that `hydo fake from-openapi` wrote answers at 127.0.0.1:9001),
//	                   then reads the payout back and answers when it exists
//	market place <n>   place order n; exit 0 when the service acknowledged it
//
// Standard library only. A create that fails with a 5xx, a 429 or no answer is tried again, up to
// three times, with the order's idempotency key; retry_buggy.go and retry_fixed.go build each try.
package main

import (
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"os"
	"strings"
	"time"
)

const api = "http://127.0.0.1:9001/v1/payouts"

var client = &http.Client{Timeout: 2 * time.Second}

type payout struct {
	ID     string `json:"id"`
	Order  string `json:"order"`
	Amount int    `json:"amount"`
	Status string `json:"status"`
}

// create pays the order's seller out and returns the payout's id.
func create(order string) (string, error) {
	body := fmt.Sprintf(`{"amount":1250,"currency":"eur","order":%q}`, order)
	key := "payout-" + order
	var last error
	for try := 1; try <= 3; try++ {
		resp, err := client.Do(newRequest(body, key, try))
		if err == nil {
			b, _ := io.ReadAll(resp.Body)
			resp.Body.Close()
			if resp.StatusCode == 201 {
				var p payout
				if json.Unmarshal(b, &p) == nil && p.ID != "" {
					return p.ID, nil
				}
				return "", fmt.Errorf("payouts: no id in %q", b)
			}
			if resp.StatusCode < 500 && resp.StatusCode != 429 {
				return "", fmt.Errorf("payouts: %d %s", resp.StatusCode, b)
			}
			last = fmt.Errorf("payouts: %d %s", resp.StatusCode, b)
		} else {
			last = err
		}
		time.Sleep(time.Duration(200*try) * time.Millisecond)
	}
	return "", last
}

// read reads the payout back, as the service does before it tells the buyer the seller is paid.
func read(id, order string) error {
	resp, err := client.Get(api + "/" + id)
	if err != nil {
		return err
	}
	b, _ := io.ReadAll(resp.Body)
	resp.Body.Close()
	if resp.StatusCode != 200 {
		return fmt.Errorf("payouts: read %s: %d %s", id, resp.StatusCode, b)
	}
	var p payout
	if err := json.Unmarshal(b, &p); err != nil {
		return fmt.Errorf("payouts: read %s: %v in %q", id, err, b)
	}
	if p.ID != id || p.Order != order || p.Amount != 1250 {
		return fmt.Errorf("payouts: read %s: another payout %q", id, b)
	}
	return nil
}

func placeOrder(w http.ResponseWriter, r *http.Request) {
	order := "o" + strings.TrimPrefix(r.URL.Path, "/orders/")
	id, err := create(order)
	if err != nil {
		http.Error(w, err.Error(), 502)
		return
	}
	if err := read(id, order); err != nil {
		http.Error(w, err.Error(), 502)
		return
	}
	fmt.Fprintf(w, "{\"order\":%q,\"payout\":%q}\n", order, id)
}

func server() {
	http.HandleFunc("POST /orders/", placeOrder)
	err := http.ListenAndServe("127.0.0.1:8080", nil)
	fmt.Fprintln(os.Stderr, err)
	os.Exit(1)
}

func place(n string) int {
	c := &http.Client{Timeout: 20 * time.Second}
	resp, err := c.Post("http://127.0.0.1:8080/orders/"+n, "text/plain", nil)
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		return 1
	}
	body, _ := io.ReadAll(resp.Body)
	resp.Body.Close()
	if resp.StatusCode != 200 {
		fmt.Fprintf(os.Stderr, "order %s: %d %s", n, resp.StatusCode, body)
		return 1
	}
	return 0
}

func main() {
	if len(os.Args) < 2 {
		fmt.Fprintln(os.Stderr, "usage: market server | place <n>")
		os.Exit(2)
	}
	switch os.Args[1] {
	case "server":
		server()
	case "place":
		if len(os.Args) != 3 {
			os.Exit(2)
		}
		os.Exit(place(os.Args[2]))
	default:
		os.Exit(2)
	}
}
