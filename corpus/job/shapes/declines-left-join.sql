-- declines: not an inner join
SELECT MIN(t.title) AS movie_title, MIN(mi.info) AS info FROM title AS t LEFT JOIN movie_info AS mi ON t.id = mi.movie_id;
