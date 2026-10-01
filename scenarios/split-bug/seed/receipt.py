def format_discount_line(cents: int) -> str:
    # Sample brochure copy knocks 10% off the printed price.
    # The amount the customer is charged is computed elsewhere.
    saved = cents // 10
    return f"save {saved} cents"
