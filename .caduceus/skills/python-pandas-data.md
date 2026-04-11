---
name: python-pandas-data
version: "1.0"
description: Data analysis workflows with pandas — loading, cleaning, transforming, aggregating, and exporting datasets
categories: [python, data, analytics]
triggers: ["pandas dataframe analysis", "pandas groupby aggregate", "pandas data cleaning", "pandas merge join", "python data analysis"]
tools: [read_file, edit_file, shell, run_tests]
---

# Pandas Data Analysis Skill

## Setup
```bash
pip install pandas numpy pyarrow openpyxl
```

## Loading Data
```python
import pandas as pd

df = pd.read_csv("data.csv", parse_dates=["created_at"])
df = pd.read_parquet("data.parquet")
df = pd.read_excel("data.xlsx", sheet_name="Sales")
df = pd.read_sql("SELECT * FROM orders WHERE date > '2024-01-01'", con=engine)
```

## First Inspection
```python
df.shape             # (rows, cols)
df.dtypes            # column types
df.describe()        # numeric statistics
df.isnull().sum()    # missing values per column
df.head(10)
df.info()            # memory usage + dtype overview
```

## Cleaning
```python
df = df.drop_duplicates(subset=["order_id"])
df["amount"] = df["amount"].fillna(0)
df = df.dropna(subset=["customer_id"])
df["price"] = pd.to_numeric(df["price"], errors="coerce")
df["date"] = pd.to_datetime(df["date"], utc=True)
df = df.rename(columns={"cust_id": "customer_id", "amt": "amount"})
```

## Transformation
```python
df["revenue"] = df["quantity"] * df["unit_price"]
df["month"] = df["date"].dt.to_period("M")
df["name_clean"] = df["name"].str.strip().str.upper()
df["tier"] = pd.cut(
    df["revenue"],
    bins=[0, 100, 1000, float("inf")],
    labels=["low", "mid", "high"],
)
```

## Aggregation
```python
summary = (
    df.groupby(["region", "month"])
    .agg(
        total_revenue=("revenue", "sum"),
        order_count=("order_id", "nunique"),
        avg_order=("revenue", "mean"),
    )
    .reset_index()
    .sort_values("total_revenue", ascending=False)
)
```

## Merging
```python
result = df_orders.merge(df_customers, on="customer_id", how="left")
result = df_a.merge(df_b, left_on="id", right_on="ref_id", how="inner")
```

## Exporting
```python
df.to_csv("output.csv", index=False)
df.to_parquet("output.parquet", index=False)
df.to_excel("output.xlsx", index=False, sheet_name="Results")
```

## Performance Tips
- Specify `dtype` in `read_csv` to avoid slow object-type columns for known fields
- Use `.query("revenue > 100")` for readable boolean filtering
- Use PyArrow backend for large datasets: `pd.read_csv(..., dtype_backend="pyarrow")`
- Use `.pipe()` chaining to keep transformation steps readable and independently testable
