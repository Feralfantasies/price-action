import os
import time
from datetime import datetime, timedelta, timezone
import requests
import pandas as pd

# 1. Credentials and asset configuration
API_KEY = os.getenv("MASSIVE_API_KEY") or os.getenv("POLYGON_API_KEY")
TICKERS = ["AAPL", "NVDA", "MSFT", "GOOGL", "TSLA"]  # Target ticker list
MULTIPLIER = 15
TIMESPAN = "minute"

if not API_KEY:
    raise ValueError("Missing MASSIVE_API_KEY environment variable.")

# 2. Date range calculation (past 7 days)
now = datetime.now(timezone.utc)
from_date = (now - timedelta(days=30)).strftime("%Y-%m-%d")
to_date = now.strftime("%Y-%m-%d")

combined_rows = []

# 3. Iterate over tickers
for ticker in TICKERS:
    url = f"https://api.massive.com/v2/aggs/ticker/{ticker}/range/{MULTIPLIER}/{TIMESPAN}/{from_date}/{to_date}"
    params = {
        "adjusted": "true",
        "sort": "asc",
        "limit": 50000,
        "apiKey": API_KEY
    }
    
    response = requests.get(url, params=params)
    data = response.json()
    
    if response.status_code == 200 and "results" in data:
        for bar in data["results"]:
            combined_rows.append({
                "ticker": ticker,
                "timestamp": int(bar["t"] / 1000),  # Milliseconds to Unix seconds
                "open": round(bar["o"], 2),
                "high": round(bar["h"], 2),
                "low": round(bar["l"], 2),
                "close": round(bar["c"], 2),
                "volume": int(bar["v"])
            })
        print(f"Successfully fetched {len(data['results'])} bars for {ticker}")
    else:
        print(f"Error fetching {ticker}: {data.get('error', data.get('status'))}")
    
    # Pause 12 seconds per call if on the free tier (5 calls/min limit)
    time.sleep(12)

# 4. Save combined CSV
df = pd.DataFrame(combined_rows)
df.to_csv("multi_ticker_15m_data.csv", index=False)
print(f"Saved total {len(df)} rows across {len(TICKERS)} tickers to 'multi_ticker_15m_data.csv'.")
