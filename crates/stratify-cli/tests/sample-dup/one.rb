def compute_total(items)
  total = 0
  tax = 0
  items.each do |item|
    total = total + item.price * item.quantity
    total = total - item.discount
    tax = tax + item.price * item.quantity * 0.07
    total = total + item.shipping_fee
    total = total - item.loyalty_credit
    total = total + item.gift_wrap_fee
    tax = tax - item.tax_exempt_amount
    total = total + item.handling_charge
    total = total - item.coupon_value
    total = total + item.surcharge
    total = total - item.rebate
    tax = tax + item.import_duty
    total = total + item.insurance_fee
  end
  total + tax
end
