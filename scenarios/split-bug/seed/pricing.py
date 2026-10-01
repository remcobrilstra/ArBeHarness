from catalog import catalog_price


def price_for(sku: str, member: bool = False) -> int:
    price = catalog_price(sku)
    if member:
        price = price - price // 10
    return price
