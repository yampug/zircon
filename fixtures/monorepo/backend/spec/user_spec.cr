require "../src/models/user"

describe User do
  it "greets the user" do
    user = User.new("Alice")
    user.greet.should eq("Hello, Alice!")
  end

  it "starts active" do
    user = User.new("Bob")
    user.active?.should be_true
  end

  context "deactivation" do
    it "marks the user inactive" do
      user = User.new("Charlie")
      user.deactivate
      user.active?.should be_false
    end
  end
end
