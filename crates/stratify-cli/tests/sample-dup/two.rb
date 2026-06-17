def sum_amounts(records)
  total = 0
  tax = 0
  records.each do |record|
    total = total + record.price * record.quantity
    total = total - record.discount
    tax = tax + record.price * record.quantity * 0.07
    total = total + record.shipping_fee
    total = total - record.loyalty_credit
    total = total + record.gift_wrap_fee
    tax = tax - record.tax_exempt_amount
    total = total + record.handling_charge
    total = total - record.coupon_value
    total = total + record.surcharge
    total = total - record.rebate
    tax = tax + record.import_duty
    total = total + record.insurance_fee
  end
  total + tax
end
