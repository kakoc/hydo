# frozen_string_literal: true

# A TCP proxy for PostgreSQL that can hold one transaction open: LISTEN_PORT -> TARGET_HOST:5432.
# It reads the client's protocol messages. When the file ARM exists and the client sends the
# UPDATE that terminates a subscription (a Query or Parse whose SQL updates "subscriptions" and
# sets "terminated_at", or a Bind of a statement prepared from such SQL), the proxy removes ARM
# and holds the server's answers on that connection for HOLD seconds. The UPDATE has run in the
# database, so the transaction stays open, with its row lock and terminated_at, until the answer
# is released and the API sends COMMIT. This is what a network outage between the API and the
# database does.

require "socket"

LISTEN_PORT = Integer(ENV.fetch("LISTEN_PORT", "5432"))
TARGET_HOST = ENV.fetch("TARGET_HOST", "db")
TARGET_PORT = Integer(ENV.fetch("TARGET_PORT", "5432"))
HOLD = Float(ENV.fetch("HOLD", "960"))
ARM = ENV.fetch("ARM", "/tmp/armed")

$stdout.sync = true
def log(msg) = puts("#{Time.now.utc.strftime("%H:%M:%S.%L")} #{msg}")

def termination_sql?(sql)
  sql.include?('UPDATE "subscriptions"') && sql.include?('"terminated_at"')
end

def handle(client)
  server = TCPSocket.new(TARGET_HOST, TARGET_PORT)
  hold_until = nil
  lock = Mutex.new
  prepared = {}
  conn = "#{client.peeraddr[3]}:#{client.peeraddr[1]}"

  trigger = lambda do |what|
    next unless File.exist?(ARM)
    secs = File.read(ARM).to_f rescue 0.0
    secs = HOLD unless secs.positive?
    File.delete(ARM) rescue nil
    lock.synchronize { hold_until = Time.now + secs }
    log "#{conn}: #{what}: holding the database's answers for #{secs.to_i} s"
  end

  # client -> server: forward at once, and parse the messages
  Thread.new do
    buf = +"".b
    started = false
    begin
      loop do
        data = client.readpartial(65_536)
        server.write(data)
        buf << data
        loop do
          if !started
            break if buf.bytesize < 8
            len = buf[0, 4].unpack1("N")
            break if buf.bytesize < len
            code = buf[4, 4].unpack1("N")
            buf = buf[len..]
            started = true unless code == 80_877_103 # SSLRequest comes before the startup message
            next
          end
          break if buf.bytesize < 5
          type = buf[0]
          len = buf[1, 4].unpack1("N")
          break if buf.bytesize < len + 1
          body = buf[5, len - 4]
          buf = buf[(len + 1)..]
          case type
          when "Q"
            trigger.call("Query") if termination_sql?(body)
          when "P"
            name, sql = body.split("\0", 3)
            if termination_sql?(sql.to_s)
              prepared[name] = true
              trigger.call("Parse #{name.inspect}")
            end
          when "B"
            _portal, stmt = body.split("\0", 3)
            trigger.call("Bind #{stmt.inspect}") if prepared[stmt] && !stmt.to_s.empty?
          end
        end
      end
    rescue EOFError, IOError, SystemCallError
      server.close rescue nil
    end
  end

  # server -> client: forward, unless the connection is held
  begin
    loop do
      data = server.readpartial(65_536)
      loop do
        wait = lock.synchronize { hold_until && hold_until - Time.now }
        break if wait.nil? || wait <= 0
        sleep [wait, 1].min
      end
      if lock.synchronize { hold_until }
        lock.synchronize { hold_until = nil }
        log "#{conn}: released the answers"
      end
      client.write(data)
    end
  rescue EOFError, IOError, SystemCallError
    client.close rescue nil
  end
end

srv = TCPServer.new("0.0.0.0", LISTEN_PORT)
log "listening on #{LISTEN_PORT}, forwarding to #{TARGET_HOST}:#{TARGET_PORT}, hold #{HOLD} s when #{ARM} exists"
loop do
  c = srv.accept
  Thread.new(c) { |cl| handle(cl) }
end
