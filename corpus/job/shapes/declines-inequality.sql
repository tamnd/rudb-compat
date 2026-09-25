-- declines: not an equality of integer columns
SELECT MIN(t1.title) AS older, MAX(t2.title) AS newer FROM title AS t1, title AS t2 WHERE t1.production_year < t2.production_year;
