-- fires
-- The same star written with JOIN ... ON rather than a comma list.
SELECT MIN(cn.name) AS company, MIN(t.title) AS movie_title FROM title AS t
JOIN movie_companies AS mc ON mc.movie_id = t.id JOIN company_name AS cn ON cn.id = mc.company_id
WHERE cn.country_code <> '[us]' AND t.production_year BETWEEN 2000 AND 2020;
