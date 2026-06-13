def classify(n)
  if n < 0
    return "negative"
  elsif n == 0
    return "zero"
  elsif n < 10
    return "small"
  elsif n < 100
    return "medium"
  elsif n < 1000
    return "large"
  elsif n < 10000
    return "huge"
  elsif n < 100000
    return "massive"
  elsif n < 1000000
    return "enormous"
  elsif n < 10000000
    return "gigantic"
  elsif n < 100000000
    return "astronomical"
  else
    return "unknown"
  end
end

classify(5)
