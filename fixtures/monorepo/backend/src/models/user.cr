class User
  getter name : String
  getter? active : Bool

  def initialize(@name : String, @active = true)
  end

  def greet
    "Hello, #{@name}!"
  end

  def deactivate
    @active = false
  end
end
