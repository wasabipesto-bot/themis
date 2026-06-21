# /// script
# requires-python = ">=3.13"
# dependencies = [
#     "dotenv>=0.9.9",
#     "pyjwt>=2.13.0",
# ]
# ///
import os
from datetime import datetime, timedelta

import jwt
from dotenv import load_dotenv

# Load environment variables from .env file
load_dotenv()

# Get JWT secret from environment variable
jwt_secret = os.environ.get("PGRST_JWT_SECRET")

if not jwt_secret:
    raise ValueError("PGRST_JWT_SECRET environment variable must be set")

payload = {"role": "admin", "exp": datetime.now() + timedelta(days=30)}

token = jwt.encode(payload, jwt_secret, algorithm="HS256")
print(token)
