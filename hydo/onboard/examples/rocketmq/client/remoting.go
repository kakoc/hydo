package main

// A small client of RocketMQ's remoting protocol for what rocketmq-client-go v2.1.2 does not
// expose: a synchronous consumer offset commit (the Go client sends it one-way), the offset query,
// a pull at an exact offset with the subscription in the request, the send-back of a message for
// a retry, the creation of topics, and the cluster's master. The frame is RocketMQ's: a 4-byte
// length, a 4-byte header length with the serialization type in its first byte (0: JSON), the
// JSON header, and the body.

import (
	"encoding/binary"
	"encoding/json"
	"fmt"
	"io"
	"net"
	"regexp"
	"sort"
	"strconv"
	"sync/atomic"
	"time"
)

const (
	codePull         = 11
	codeQueryOffset  = 14
	codeUpdateOffset = 15
	codeCreateTopic  = 17
	codeSendBack     = 36
	codeClusterInfo  = 106
	resSuccess       = 0
	resPullNotFound  = 19
	resPullRetry     = 20
	resPullMoved     = 21
	resQueryNotFound = 22
	namesrvAddr      = "10.89.0.10:9876"
	brokerName       = "broker-a"
)

var opaque int32

type header struct {
	Code      int               `json:"code"`
	Language  string            `json:"language"`
	Version   int               `json:"version"`
	Opaque    int32             `json:"opaque"`
	Flag      int               `json:"flag"`
	Remark    string            `json:"remark,omitempty"`
	ExtFields map[string]string `json:"extFields"`
}

type reply struct {
	code   int
	remark string
	ext    map[string]string
	body   []byte
}

// call sends one request to addr and waits for its response.
func call(addr string, code int, ext map[string]string, body []byte, timeout time.Duration) (*reply, error) {
	conn, err := net.DialTimeout("tcp", addr, timeout)
	if err != nil {
		return nil, err
	}
	defer conn.Close()
	conn.SetDeadline(time.Now().Add(timeout))
	h := header{Code: code, Language: "GO", Version: 431, Opaque: atomic.AddInt32(&opaque, 1), ExtFields: ext}
	hb, err := json.Marshal(h)
	if err != nil {
		return nil, err
	}
	frame := make([]byte, 8, 8+len(hb)+len(body))
	binary.BigEndian.PutUint32(frame[0:4], uint32(4+len(hb)+len(body)))
	binary.BigEndian.PutUint32(frame[4:8], uint32(len(hb))) // serialization type 0: JSON
	frame = append(frame, hb...)
	frame = append(frame, body...)
	if _, err := conn.Write(frame); err != nil {
		return nil, err
	}
	for {
		var lb [4]byte
		if _, err := io.ReadFull(conn, lb[:]); err != nil {
			return nil, err
		}
		n := binary.BigEndian.Uint32(lb[:])
		if n < 4 || n > 64<<20 {
			return nil, fmt.Errorf("bad frame length %d", n)
		}
		buf := make([]byte, n)
		if _, err := io.ReadFull(conn, buf); err != nil {
			return nil, err
		}
		hl := binary.BigEndian.Uint32(buf[0:4]) & 0xffffff
		if buf[0] != 0 || int(hl)+4 > len(buf) {
			return nil, fmt.Errorf("unsupported header (type %d, length %d)", buf[0], hl)
		}
		var rh header
		if err := json.Unmarshal(buf[4:4+hl], &rh); err != nil {
			return nil, fmt.Errorf("bad header: %v", err)
		}
		if rh.Flag&1 == 0 || rh.Opaque != h.Opaque {
			continue // a request of the broker to us, or another answer: not ours
		}
		return &reply{code: rh.Code, remark: rh.Remark, ext: rh.ExtFields, body: buf[4+hl:]}, nil
	}
}

var addrRe = regexp.MustCompile(`"?(-?\d+)"?\s*:\s*"(\d+\.\d+\.\d+\.\d+:\d+)"`)
var brokerAddrsRe = regexp.MustCompile(`"brokerAddrs"\s*:\s*\{([^}]*)\}`)

// brokers asks the NameServer for broker-a's addresses: brokerId -> address (0 is the master).
// The body is fastjson's, whose map keys can be numbers without quotes, so it is read with a
// regular expression.
func brokers() (map[int]string, error) {
	r, err := call(namesrvAddr, codeClusterInfo, map[string]string{}, nil, 5*time.Second)
	if err != nil {
		return nil, err
	}
	if r.code != resSuccess {
		return nil, fmt.Errorf("cluster info: code %d %s", r.code, r.remark)
	}
	out := map[int]string{}
	for _, m := range brokerAddrsRe.FindAllSubmatch(r.body, -1) {
		for _, p := range addrRe.FindAllSubmatch(m[1], -1) {
			id, _ := strconv.Atoi(string(p[1]))
			out[id] = string(p[2])
		}
	}
	return out, nil
}

// master returns the address of broker-a's master, as the NameServer knows it.
func master() (string, error) {
	b, err := brokers()
	if err != nil {
		return "", err
	}
	if a, ok := b[0]; ok {
		return a, nil
	}
	ids := []int{}
	for id := range b {
		ids = append(ids, id)
	}
	sort.Ints(ids)
	return "", fmt.Errorf("no master (brokers %v)", ids)
}

func queryOffset(addr, group, topic string, q int) (int64, bool, error) {
	r, err := call(addr, codeQueryOffset, map[string]string{
		"consumerGroup": group, "topic": topic, "queueId": strconv.Itoa(q), "setZeroIfNotFound": "false",
	}, nil, 5*time.Second)
	if err != nil {
		return 0, false, err
	}
	switch r.code {
	case resSuccess:
		o, err := strconv.ParseInt(r.ext["offset"], 10, 64)
		return o, true, err
	case resQueryNotFound:
		return 0, false, nil
	}
	return 0, false, fmt.Errorf("query offset: code %d %s", r.code, r.remark)
}

func updateOffset(addr, group, topic string, q int, off int64) error {
	r, err := call(addr, codeUpdateOffset, map[string]string{
		"consumerGroup": group, "topic": topic, "queueId": strconv.Itoa(q), "commitOffset": strconv.FormatInt(off, 10),
	}, nil, 5*time.Second)
	if err != nil {
		return err
	}
	if r.code != resSuccess {
		return fmt.Errorf("update offset: code %d %s", r.code, r.remark)
	}
	return nil
}

type pulled struct {
	code     int
	next     int64
	min, max int64
	msgs     []*msgExt
	remark   string
}

// pull reads up to n messages of a queue from offset, with the subscription "*" in the request.
func pull(addr, group, topic string, q int, offset int64, n int) (*pulled, error) {
	r, err := call(addr, codePull, map[string]string{
		"consumerGroup": group, "topic": topic, "queueId": strconv.Itoa(q),
		"queueOffset": strconv.FormatInt(offset, 10), "maxMsgNums": strconv.Itoa(n),
		"sysFlag":      "4", // FLAG_SUBSCRIPTION: the subscription is in the request
		"commitOffset": "0", "suspendTimeoutMillis": "0", "subscription": "*", "subVersion": "0",
		"expressionType": "TAG",
	}, nil, 10*time.Second)
	if err != nil {
		return nil, err
	}
	p := &pulled{code: r.code, remark: r.remark}
	p.next, _ = strconv.ParseInt(r.ext["nextBeginOffset"], 10, 64)
	p.min, _ = strconv.ParseInt(r.ext["minOffset"], 10, 64)
	p.max, _ = strconv.ParseInt(r.ext["maxOffset"], 10, 64)
	switch r.code {
	case resSuccess:
		p.msgs = decode(r.body)
	case resPullNotFound, resPullRetry, resPullMoved:
	default:
		return p, fmt.Errorf("pull: code %d %s", r.code, r.remark)
	}
	return p, nil
}

// sendBack hands a message back to the broker for a retry (or the dead-letter topic after
// maxReconsume deliveries), as a consumer's RECONSUME_LATER does.
func sendBack(addr, group string, m *msgExt, delayLevel, maxReconsume int) error {
	origin := m.props["ORIGIN_MESSAGE_ID"]
	if origin == "" {
		origin = m.msgID
	}
	topic := m.props["RETRY_TOPIC"]
	if topic == "" {
		topic = m.topic
	}
	r, err := call(addr, codeSendBack, map[string]string{
		"offset": strconv.FormatInt(m.commitLogOffset, 10), "group": group,
		"delayLevel": strconv.Itoa(delayLevel), "originMsgId": origin, "originTopic": topic,
		"unitMode": "false", "maxReconsumeTimes": strconv.Itoa(maxReconsume),
	}, nil, 10*time.Second)
	if err != nil {
		return err
	}
	if r.code != resSuccess {
		return fmt.Errorf("send back: code %d %s", r.code, r.remark)
	}
	return nil
}

func createTopic(addr, topic string, queues, perm int) error {
	r, err := call(addr, codeCreateTopic, map[string]string{
		"topic": topic, "defaultTopic": "TBW102", "readQueueNums": strconv.Itoa(queues),
		"writeQueueNums": strconv.Itoa(queues), "perm": strconv.Itoa(perm), "topicFilterType": "SINGLE_TAG",
		"topicSysFlag": "0", "order": "false",
	}, nil, 10*time.Second)
	if err != nil {
		return err
	}
	if r.code != resSuccess {
		return fmt.Errorf("create topic %s: code %d %s", topic, r.code, r.remark)
	}
	return nil
}
