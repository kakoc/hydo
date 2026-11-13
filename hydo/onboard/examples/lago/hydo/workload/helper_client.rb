# frozen_string_literal: true

# The test's Lago client: every workload command and check calls it through the API image's own
# Ruby (`hydo-oci api ruby /hydo/workload/helper_client.rb <command> ...`), so it needs nothing
# beyond Ruby's standard library. It speaks Lago's public REST API (/api/v1) with the
# organization's API key, as a customer's billing integration does.
#
# Commands:
#   seed                         the catalogue: two billable metrics, one plan, CUSTOMERS customers,
#                                each with one subscription that started on 2026-11-01
#   event <op> <sub> <code> <n>  send one usage event (transaction id <op>), retrying a lost answer
#   batch <op> <sub> <code> <n>... send several events in one request (ids <op>.1, <op>.2, ...)
#   terminate <sub>              end a subscription now (Lago then bills it on a termination invoice)
#   report                       print what Lago reports, one "<key> <amount>" line per key
#   wait                         exit 0 once the API answers
#
# Exit status of event and batch: 0 acknowledged, 1 failed for certain (Lago refused it), 3 no
# answer (unknown: Lago may or may not have stored it).

require "json"
require "net/http"
require "uri"

API = URI(ENV.fetch("LAGO_URL", "http://10.89.0.14:3000"))
KEY = ENV.fetch("LAGO_KEY", "hydo-test-api-key")
CUSTOMERS = 4
SUBSCRIPTION_AT = "2026-11-01T00:00:00Z"

class NoAnswer < StandardError; end

def request(method, path, body = nil, timeout: 10)
  http = Net::HTTP.new(API.host, API.port)
  http.open_timeout = 3
  http.read_timeout = timeout
  http.write_timeout = timeout
  req = Net::HTTP.const_get(method.capitalize).new(path)
  req["Authorization"] = "Bearer #{KEY}"
  req["Content-Type"] = "application/json"
  req.body = JSON.generate(body) if body
  res = http.request(req)
  data = begin
    res.body.to_s.empty? ? {} : JSON.parse(res.body)
  rescue JSON::ParserError
    {"raw" => res.body.to_s[0, 200]}
  end
  [res.code.to_i, data]
rescue Timeout::Error, SystemCallError, IOError, EOFError, Net::HTTPBadResponse, SocketError => e
  raise NoAnswer, e.class.name
end

# A transaction id that Lago already holds means an earlier attempt of the same request
# was stored: the client counts that as acknowledged (Lago documents transaction_id as the
# deduplication key of events).
def duplicate?(data)
  data.to_s.include?("value_already_exist")
end

# Send one request; retry a lost answer or a server error up to 3 times with the same ids.
def send_with_retries(method, path, body)
  last = :unknown
  3.times do |attempt|
    sleep(0.5 * attempt) if attempt.positive?
    begin
      code, data = request(method, path, body)
    rescue NoAnswer => e
      warn "attempt #{attempt + 1}: no answer (#{e.message})"
      last = :unknown
      next
    end
    warn "attempt #{attempt + 1}: HTTP #{code} #{JSON.generate(data)[0, 300]}"
    return :ack if code == 200
    return :ack if code == 422 && duplicate?(data)
    if code >= 500 || code == 429
      last = :unknown
      next
    end
    return :failed if code == 422 || code == 400 || code == 404
    last = :unknown
  end
  last
end

def outcome_exit(outcome)
  puts outcome
  exit({ack: 0, failed: 1, unknown: 3}.fetch(outcome))
end

def seed
  must = lambda do |method, path, body|
    code, data = request(method, path, body, timeout: 60)
    ok = code == 200 || (code == 422 && data.to_s.include?("value_already_exist"))
    abort "seed: #{method} #{path}: HTTP #{code} #{data}" unless ok
    data
  end
  must.call(:post, "/api/v1/billable_metrics", {billable_metric: {
    name: "API calls", code: "api_calls", aggregation_type: "sum_agg", field_name: "units"
  }})
  must.call(:post, "/api/v1/billable_metrics", {billable_metric: {
    name: "Transfers", code: "transfers", aggregation_type: "sum_agg", field_name: "units"
  }})
  code, metrics = request(:get, "/api/v1/billable_metrics?per_page=100", nil, timeout: 60)
  abort "seed: billable metrics: HTTP #{code}" unless code == 200
  id = metrics["billable_metrics"].to_h { |m| [m["code"], m["lago_id"]] }
  must.call(:post, "/api/v1/plans", {plan: {
    name: "Usage", code: "usage", interval: "monthly", amount_cents: 1000, amount_currency: "USD",
    pay_in_advance: false,
    charges: [
      # in arrears: billed on the invoice at the end of the month, 10 cents per unit
      {billable_metric_id: id.fetch("api_calls"), charge_model: "standard", pay_in_advance: false,
       properties: {amount: "0.10"}},
      # in advance: every event is charged at once on its own invoice, 1 USD per unit
      {billable_metric_id: id.fetch("transfers"), charge_model: "standard", pay_in_advance: true,
       invoiceable: true, properties: {amount: "1"}}
    ]
  }})
  (1..CUSTOMERS).each do |i|
    must.call(:post, "/api/v1/customers", {customer: {
      external_id: "cust#{i}", name: "Customer #{i}", currency: "USD"
    }})
    must.call(:post, "/api/v1/subscriptions", {subscription: {
      external_customer_id: "cust#{i}", plan_code: "usage", external_id: "sub#{i}",
      billing_time: "calendar", subscription_at: SUBSCRIPTION_AT
    }})
  end
  puts "seeded #{CUSTOMERS} customers"
end

def event(op, sub, code, units)
  body = {event: {transaction_id: op, external_subscription_id: sub, code: code,
                  properties: {units: units.to_i}}}
  outcome_exit(send_with_retries(:post, "/api/v1/events", body))
end

def batch(op, sub, code, *units)
  events = units.each_with_index.map do |n, i|
    {transaction_id: "#{op}.#{i + 1}", external_subscription_id: sub, code: code,
     properties: {units: n.to_i}}
  end
  outcome_exit(send_with_retries(:post, "/api/v1/events/batch", {events: events}))
end

# A subscription that is no longer active answers 404: an earlier attempt (or another driver)
# ended it, which is the outcome the operation asks for.
def terminate(sub)
  outcome = :unknown
  3.times do |attempt|
    sleep(0.5 * attempt) if attempt.positive?
    begin
      code, data = request(:delete, "/api/v1/subscriptions/#{sub}")
    rescue NoAnswer => e
      warn "attempt #{attempt + 1}: no answer (#{e.message})"
      next
    end
    warn "attempt #{attempt + 1}: HTTP #{code} #{JSON.generate(data)[0, 300]}"
    if code == 200 || code == 404
      outcome = :ack
      break
    end
    if code < 500
      outcome = :failed
      break
    end
  end
  outcome_exit(outcome)
end

def get_all(path, key)
  out = []
  page = 1
  loop do
    sep = path.include?("?") ? "&" : "?"
    code, data = request(:get, "#{path}#{sep}per_page=100&page=#{page}", nil, timeout: 60)
    raise NoAnswer, "HTTP #{code} on #{path}" unless code == 200
    out.concat(data.fetch(key))
    break if data.dig("meta", "next_page").nil?
    page += 1
  end
  out
end

# What Lago reports to the customer, as "<key> <amount>" lines:
#   units:<sub>:api_calls  units of api_calls billed on every invoice plus the current period's usage
#   units:<sub>:transfers  units of transfers charged in advance (one fee per event)
def report
  totals = Hash.new(0)
  (1..CUSTOMERS).each do |i|
    sub = "sub#{i}"
    invoices = get_all("/api/v1/invoices?external_customer_id=cust#{i}", "invoices")
    invoices.each do |inv|
      next if inv["status"] == "voided"
      code, full = request(:get, "/api/v1/invoices/#{inv["lago_id"]}", nil, timeout: 60)
      raise NoAnswer, "HTTP #{code} on invoice" unless code == 200
      full["invoice"].fetch("fees", []).each do |fee|
        metric = fee.dig("item", "code")
        next unless %w[api_calls transfers].include?(metric)
        totals["units:#{sub}:#{metric}"] += fee["units"].to_f
      end
    end
    code, usage = request(:get, "/api/v1/customers/cust#{i}/current_usage?external_subscription_id=#{sub}",
      nil, timeout: 60)
    # an ended subscription has no period in progress: Lago answers 405 no_active_subscription
    next if code == 405 && usage.to_s.include?("no_active_subscription")
    raise NoAnswer, "HTTP #{code} on current usage" unless code == 200
    usage.dig("customer_usage", "charges_usage").to_a.each do |cu|
      metric = cu.dig("billable_metric", "code")
      next unless metric == "api_calls"
      totals["units:#{sub}:#{metric}"] += cu["units"].to_f
    end
  end
  totals.sort.each { |k, v| puts "#{k} #{v.round(6) == v.round ? v.round : v}" }
rescue NoAnswer => e
  warn "report: #{e.message}"
  exit 2
end

def wait_api
  code, = request(:get, "/health")
  exit(code == 200 ? 0 : 1)
rescue NoAnswer
  exit 1
end

cmd, *args = ARGV
case cmd
when "seed" then seed
when "event" then event(*args)
when "batch" then batch(*args)
when "terminate" then terminate(*args)
when "report" then report
when "wait" then wait_api
else abort "usage: helper_client.rb seed|event|batch|terminate|report|wait"
end
