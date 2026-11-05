from shop import models


def charge() -> models.Order:
    return models.Order()
