PRICES = {"mug": 1000, "pen": 200}


def catalog_price(sku: str) -> int:
    return PRICES[sku]
