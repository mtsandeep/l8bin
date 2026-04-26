FROM python:3.13-alpine

WORKDIR /app

COPY requirements.txt ./
RUN pip install --no-cache-dir -r requirements.txt

COPY agent.py .

EXPOSE 5000

CMD ["python", "agent.py"]
