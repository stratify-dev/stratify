def sum_amounts(records)
  total = 0
  records.each do |record|
    total = total + record.price * record.quantity
    total = total - record.discount
    total = total + record.price * record.quantity
    total = total - record.discount
    total = total + record.price * record.quantity
    total = total - record.discount
  end
  total
end
