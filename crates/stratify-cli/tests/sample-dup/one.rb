def compute_total(items)
  total = 0
  items.each do |item|
    total = total + item.price * item.quantity
    total = total - item.discount
    total = total + item.price * item.quantity
    total = total - item.discount
    total = total + item.price * item.quantity
    total = total - item.discount
  end
  total
end
