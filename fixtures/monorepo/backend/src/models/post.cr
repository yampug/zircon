class Post
  property title : String
  property body : String
  getter author : User

  MAX_TITLE_LENGTH = 255

  alias Tags = Array(String)

  def initialize(@title : String, @body : String, @author : User)
  end

  def summary
    "#{@title} by #{@author.name}"
  end
end
