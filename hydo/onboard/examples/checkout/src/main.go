// checkout: a small order service for hydo's emulators example (hydo.yaml next to src/).
//
//	checkout server     take orders on 127.0.0.1:8080: POST /orders/<n> charges the order through
//	                    Stripe (https://api.stripe.com, the stripe emulator), keeps its receipt in
//	                    S3 (bucket receipts, the s3 emulator at $AWS_ENDPOINT_URL_S3), and mails the
//	                    receipt over SMTP (127.0.0.1:1025, the smtp emulator)
//	checkout place <n>  place order n; exit 0 when the service acknowledged it
//	checkout check      every order in $HYDO_ACKED has its receipt in S3 and in the mailbox
//
// Standard library only. A charge that fails with a 5xx, a 429 or no answer is tried again, up to
// three times; key_buggy.go and key_fixed.go choose the idempotency key of each try.
package main

import (
	"bufio"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/smtp"
	"net/url"
	"os"
	"strings"
	"time"
)

const (
	stripeURL = "https://api.stripe.com/v1/payment_intents"
	stripeKey = "sk_test_hydo"
	smtpAddr  = "127.0.0.1:1025"
	// what the checks read: the emulators' own ports, not their fault proxies, so that the
	// checks do not add to the record
	s3Direct   = "http://127.0.0.1:19000"
	mailpitAPI = "http://127.0.0.1:8025"
)

var client = &http.Client{Timeout: 2 * time.Second}

// charge takes the order's amount through Stripe and returns the payment intent's id.
func charge(order string) (string, error) {
	var last error
	for try := 1; try <= 3; try++ {
		form := url.Values{}
		form.Set("amount", "1250")
		form.Set("currency", "eur")
		form.Set("confirm", "true")
		form.Set("payment_method", "pm_card_visa")
		form.Set("metadata[order]", order)
		req, _ := http.NewRequest("POST", stripeURL, strings.NewReader(form.Encode()))
		req.Header.Set("Content-Type", "application/x-www-form-urlencoded")
		req.Header.Set("Authorization", "Bearer "+stripeKey)
		req.Header.Set("Idempotency-Key", idempotencyKey(order, try))
		resp, err := client.Do(req)
		if err == nil {
			body, _ := io.ReadAll(resp.Body)
			resp.Body.Close()
			if resp.StatusCode == 200 {
				var pi struct {
					ID string `json:"id"`
				}
				if json.Unmarshal(body, &pi) == nil && pi.ID != "" {
					return pi.ID, nil
				}
				return "", fmt.Errorf("stripe: no id in %q", body)
			}
			if resp.StatusCode < 500 && resp.StatusCode != 429 {
				return "", fmt.Errorf("stripe: %d %s", resp.StatusCode, body)
			}
			last = fmt.Errorf("stripe: %d", resp.StatusCode)
		} else {
			last = err
		}
		time.Sleep(time.Duration(200*try) * time.Millisecond)
	}
	return "", last
}

// storeReceipt puts the receipt into the bucket receipts. rclone's S3 server runs without keys,
// so the request is not signed; an AWS SDK's signed request is accepted as well.
func storeReceipt(order, text string) error {
	base := os.Getenv("AWS_ENDPOINT_URL_S3")
	req, _ := http.NewRequest("PUT", base+"/receipts/"+order, strings.NewReader(text))
	req.Header.Set("Content-Type", "text/plain")
	resp, err := client.Do(req)
	if err != nil {
		return err
	}
	io.Copy(io.Discard, resp.Body)
	resp.Body.Close()
	if resp.StatusCode != 200 {
		return fmt.Errorf("s3: %d", resp.StatusCode)
	}
	return nil
}

func mailReceipt(order, text string) error {
	msg := "From: shop@example.com\r\nTo: customer@example.com\r\nSubject: Receipt " + order +
		"\r\nX-Order: " + order + "\r\n\r\n" + text + "\r\n"
	return smtp.SendMail(smtpAddr, nil, "shop@example.com", []string{"customer@example.com"}, []byte(msg))
}

func placeOrder(w http.ResponseWriter, r *http.Request) {
	order := "o" + strings.TrimPrefix(r.URL.Path, "/orders/")
	pi, err := charge(order)
	if err != nil {
		http.Error(w, err.Error(), 502)
		return
	}
	text := fmt.Sprintf("order %s paid, %s", order, pi)
	if err := storeReceipt(order, text); err != nil {
		http.Error(w, err.Error(), 502)
		return
	}
	if err := mailReceipt(order, text); err != nil {
		http.Error(w, err.Error(), 502)
		return
	}
	fmt.Fprintf(w, "{\"order\":%q,\"payment\":%q}\n", order, pi)
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

// mails counts the messages in the mailbox whose subject is "Receipt <order>".
func mails(order string) (int, error) {
	q := url.QueryEscape(`subject:"Receipt ` + order + `"`)
	resp, err := client.Get(mailpitAPI + "/api/v1/search?query=" + q)
	if err != nil {
		return 0, err
	}
	defer resp.Body.Close()
	var r struct {
		Count int `json:"messages_count"`
	}
	if err := json.NewDecoder(resp.Body).Decode(&r); err != nil {
		return 0, err
	}
	return r.Count, nil
}

func check() int {
	f, err := os.Open(os.Getenv("HYDO_ACKED"))
	if err != nil && !errors.Is(err, os.ErrNotExist) {
		fmt.Println("cannot read the acknowledged orders:", err)
		return 2
	}
	n := 0
	if f != nil {
		sc := bufio.NewScanner(f)
		for sc.Scan() {
			id := strings.TrimSpace(sc.Text())
			if id == "" {
				continue
			}
			order := "o" + id
			resp, err := client.Get(s3Direct + "/receipts/" + order)
			if err != nil {
				fmt.Println("s3:", err)
				return 2
			}
			resp.Body.Close()
			if resp.StatusCode != 200 {
				fmt.Printf("order %s was acknowledged but its receipt is not in S3 (%d)\n", order, resp.StatusCode)
				return 1
			}
			k, err := mails(order)
			if err != nil {
				fmt.Println("mailbox:", err)
				return 2
			}
			if k == 0 {
				fmt.Printf("order %s was acknowledged but its receipt was never mailed\n", order)
				return 1
			}
			n++
		}
	}
	fmt.Printf("%d acknowledged orders: each receipt in S3 and in the mailbox\n", n)
	return 0
}

func main() {
	if len(os.Args) < 2 {
		fmt.Fprintln(os.Stderr, "usage: checkout server | place <n> | check")
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
	case "check":
		os.Exit(check())
	default:
		os.Exit(2)
	}
}
