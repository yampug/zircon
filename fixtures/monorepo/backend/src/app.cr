require "./models/user"
require "./models/post"

module App
  VERSION = "0.1.0"

  class Server
    @port : Int32
    @@instance_count = 0

    property host : String

    def initialize(@host = "localhost", @port = 8080)
      @@instance_count += 1
    end

    def start
      user = User.new("admin")
      puts "Starting #{user.name} server on #{@host}:#{@port}"
    end

    abstract def handle_request

    def self.instance_count
      @@instance_count
    end
  end
end
