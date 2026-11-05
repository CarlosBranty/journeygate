import os

import requests
from shop.orders import service

print(os.getcwd(), requests, service.place_order())
